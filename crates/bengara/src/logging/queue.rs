//! 出し先ごとのキューと、捨てた数（`docs/logging.md` の「溢れたとき」）。
//!
//! - 入れるのは `try_send` だけです。入らなければ捨てて数えます。イベントを出した側は待ちません。
//! - 書き手のスレッドで出たイベントは、層の側で捨てます（[`in_writer`]）。
//!   自分のログが自分に戻る輪を作らないためです。

use std::cell::Cell;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// キューの長さ（出し先ごと）。
pub(crate) const CAPACITY: usize = 8_192;

/// 書けなかったことを標準エラーに知らせる間隔（出し先ごと）。
const NOTIFY_EVERY_MILLIS: i64 = 60_000;

thread_local! {
    /// このスレッドが書き手か。書き手で出たイベントは捨てます。
    static IN_WRITER: Cell<bool> = const { Cell::new(false) };
}

/// いまのスレッドが書き手か。
pub(crate) fn in_writer() -> bool {
    IN_WRITER.with(Cell::get)
}

/// いまのスレッドを書き手として印を付ける。書き手のスレッドの最初に呼びます。
pub(crate) fn mark_writer() {
    IN_WRITER.with(|flag| flag.set(true));
}

/// キューに流すもの。
pub(crate) enum Message<T> {
    Record(T),
    /// ここまでを書き切ったら返事をする（終わるとき・`serve` の行の前）。
    Flush(std::sync::mpsc::Sender<()>),
}

/// 出し先ごとの数。捨てた数と書けなかった数を足して持ちます（メトリクスの `dropped`）。
#[derive(Default)]
pub(crate) struct Counters {
    dropped: AtomicU64,
    last_notice: AtomicI64,
}

impl Counters {
    /// 捨てた（書けなかった）数を足す。
    pub(crate) fn add_dropped(&self, count: u64) {
        self.dropped.fetch_add(count, Ordering::Relaxed);
    }

    /// 数を取り出して 0 に戻す（メトリクスの見回り）。
    pub(crate) fn take_dropped(&self) -> u64 {
        self.dropped.swap(0, Ordering::Relaxed)
    }

    /// 書けなかったことを標準エラーに知らせる。60 秒に 1 回までです。
    ///
    /// `tracing` は使いません。書き手のスレッドから呼ぶので、ログに戻すと輪になります。
    pub(crate) fn notify(&self, sink: &str, message: &str) {
        let now = crate::support::time::now_millis();
        let last = self.last_notice.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < NOTIFY_EVERY_MILLIS {
            return;
        }
        if self
            .last_notice
            .compare_exchange(last, now.max(1), Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            eprintln!("bengara: ログ（{sink}）を書けませんでした: {message}");
        }
    }
}

/// 出し先のキュー（入れる側）。
pub(crate) struct Queue<T> {
    sender: SyncSender<Message<T>>,
    counters: Arc<Counters>,
}

/// キューを作る。受け取る側は書き手のスレッドへ渡します。
pub(crate) fn channel<T>(capacity: usize) -> (Queue<T>, Receiver<Message<T>>, Arc<Counters>) {
    let (sender, receiver) = sync_channel(capacity);
    let counters = Arc::new(Counters::default());
    (
        Queue {
            sender,
            counters: counters.clone(),
        },
        receiver,
        counters,
    )
}

impl<T> Queue<T> {
    /// 入れる。入らなければ捨てて数えます。待ちません。
    pub(crate) fn push(&self, record: T) {
        if self.sender.try_send(Message::Record(record)).is_err() {
            self.counters.add_dropped(1);
        }
    }

    pub(crate) fn counters(&self) -> &Counters {
        &self.counters
    }

    /// ここまでに入れたものを書き切るのを待つ。`deadline` を過ぎたら諦めて `false`。
    ///
    /// キューが一杯でも、書き手が減らすのを待って入れます（上限までに限る）。
    pub(crate) fn flush(&self, deadline: Instant) -> bool {
        let (done, wait) = std::sync::mpsc::channel();
        let mut message = Message::Flush(done);
        loop {
            match self.sender.try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                    message = back;
                    std::thread::sleep(Duration::from_millis(5));
                }
                // 書き手がもういない。
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match wait.recv_timeout(left) {
            Ok(()) => true,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 一杯なら捨てて数える() {
        let (queue, receiver, counters) = channel::<u32>(2);
        for n in 0..5 {
            queue.push(n);
        }
        assert_eq!(counters.take_dropped(), 3);
        // 取り出すと 0 に戻る。
        assert_eq!(counters.take_dropped(), 0);
        let got: Vec<u32> = receiver
            .try_iter()
            .filter_map(|m| match m {
                Message::Record(n) => Some(n),
                Message::Flush(_) => None,
            })
            .collect();
        assert_eq!(got, vec![0, 1]);
    }

    #[test]
    fn 書き手がいなければ書き切りを待たない() {
        let (queue, receiver, _) = channel::<u32>(2);
        drop(receiver);
        assert!(!queue.flush(Instant::now() + Duration::from_secs(5)));
    }

    #[test]
    fn 書き手が返事をすれば書き切りが終わる() {
        let (queue, receiver, _) = channel::<u32>(1);
        queue.push(1);
        let writer = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for message in receiver {
                match message {
                    Message::Record(n) => seen.push(n),
                    Message::Flush(done) => {
                        let _ = done.send(());
                        break;
                    }
                }
            }
            seen
        });
        // 一杯（長さ 1）でも、書き手が減らせば入る。
        assert!(queue.flush(Instant::now() + Duration::from_secs(5)));
        assert_eq!(writer.join().unwrap(), vec![1]);
    }

    #[test]
    fn 書き手の印はスレッドごと() {
        assert!(!in_writer());
        std::thread::spawn(|| {
            mark_writer();
            assert!(in_writer());
        })
        .join()
        .unwrap();
        assert!(!in_writer());
    }
}
