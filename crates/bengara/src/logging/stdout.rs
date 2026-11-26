//! 標準出力の書き手。
//!
//! - ふだんは、イベントを出したスレッドでその場で書きます。
//! - `serve` だけは裏のスレッドで書きます（[`Stdout::start_background`]）。
//!   リクエストを処理するスレッドを、端末やパイプへの書き込みで待たせないためです。

use std::io::Write as _;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use super::queue::{self, Counters, Message, Queue};

/// 1 回にまとめて書く量の目安。
const BATCH_BYTES: usize = 64 * 1024;

/// 色を付けるか。標準出力が端末で、かつ `NO_COLOR` が無いか空のときだけ付けます。
///
/// `NO_COLOR` は <https://no-color.org/> の決まりです（中身が空でなければ、値に関係なく消す）。
pub(crate) fn use_color(is_terminal: bool, no_color: Option<&std::ffi::OsStr>) -> bool {
    is_terminal && no_color.is_none_or(|value| value.is_empty())
}

/// 書き先。
#[derive(Clone)]
enum Target {
    Real,
    /// テストで中身を確かめるための置き場所。
    #[cfg(test)]
    Capture(Arc<std::sync::Mutex<Vec<u8>>>),
}

impl Target {
    fn write(&self, bytes: &[u8]) {
        match self {
            Target::Real => {
                let mut out = std::io::stdout().lock();
                // 書けなくても（パイプが閉じたなど）本体は止めない。
                let _ = out.write_all(bytes);
                let _ = out.flush();
            }
            #[cfg(test)]
            Target::Capture(buffer) => {
                if let Ok(mut buffer) = buffer.lock() {
                    buffer.extend_from_slice(bytes);
                }
            }
        }
    }
}

/// 標準出力。
pub(crate) struct Stdout {
    target: Target,
    queue: OnceLock<Queue<String>>,
}

impl Stdout {
    pub(crate) fn real() -> Self {
        Self {
            target: Target::Real,
            queue: OnceLock::new(),
        }
    }

    /// テスト用。書いた中身を `buffer` に溜めます。
    #[cfg(test)]
    pub(crate) fn capture(buffer: Arc<std::sync::Mutex<Vec<u8>>>) -> Self {
        Self {
            target: Target::Capture(buffer),
            queue: OnceLock::new(),
        }
    }

    /// 1 行を書く。裏で書いているときはキューへ入れます（入らなければ捨てて数える）。
    pub(crate) fn write(&self, line: String) {
        match self.queue.get() {
            Some(queue) => queue.push(line),
            None => {
                // 書き手のスレッドの中で書くことはない（層が先に捨てる）ので、ここは常にその場で書く。
                self.target.write(line.as_bytes());
            }
        }
    }

    /// 裏のスレッドで書き始める。2 回目からは何もしません。
    pub(crate) fn start_background(&self, capacity: usize) {
        if self.queue.get().is_some() {
            return;
        }
        let (queue, receiver, counters) = queue::channel(capacity);
        let target = self.target.clone();
        let spawned = std::thread::Builder::new()
            .name("bengara-log-stdout".into())
            .spawn(move || run(target, receiver, counters));
        match spawned {
            Ok(_) => {
                let _ = self.queue.set(queue);
            }
            Err(error) => eprintln!(
                "bengara: 標準出力の書き手を立てられませんでした（その場で書きます）: {error}"
            ),
        }
    }

    /// 裏で書いているなら、ここまでを書き切るのを待つ。
    pub(crate) fn flush(&self, deadline: Instant) {
        if let Some(queue) = self.queue.get() {
            queue.flush(deadline);
        }
    }

    /// 捨てた数を取り出して 0 に戻す。
    pub(crate) fn take_dropped(&self) -> u64 {
        self.queue
            .get()
            .map_or(0, |queue| queue.counters().take_dropped())
    }
}

/// 裏の書き手。届いた行をまとめて書きます。
fn run(target: Target, receiver: Receiver<Message<String>>, _counters: Arc<Counters>) {
    queue::mark_writer();
    let mut batch: Vec<u8> = Vec::with_capacity(BATCH_BYTES);
    while let Ok(first) = receiver.recv() {
        let mut acks = Vec::new();
        let mut take = |message: Message<String>, batch: &mut Vec<u8>| match message {
            Message::Record(line) => batch.extend_from_slice(line.as_bytes()),
            Message::Flush(done) => acks.push(done),
        };
        take(first, &mut batch);
        // 溜まっている分もまとめて書く。
        while batch.len() < BATCH_BYTES {
            match receiver.try_recv() {
                Ok(message) => take(message, &mut batch),
                Err(_) => break,
            }
        }
        if !batch.is_empty() {
            target.write(&batch);
            batch.clear();
        }
        for done in acks {
            let _ = done.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn 色の判定() {
        // (端末か, NO_COLOR, 付けるか)
        let cases: [(bool, Option<&str>, bool); 6] = [
            (true, None, true),
            (true, Some(""), true),
            (true, Some("1"), false),
            (true, Some("0"), false),
            (false, None, false),
            (false, Some("1"), false),
        ];
        for (terminal, no_color, expected) in cases {
            assert_eq!(
                use_color(terminal, no_color.map(OsStr::new)),
                expected,
                "{terminal} {no_color:?}"
            );
        }
    }

    #[test]
    fn その場で書く() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let out = Stdout::capture(buffer.clone());
        out.write("a\n".into());
        out.write("b\n".into());
        assert_eq!(&*buffer.lock().unwrap(), b"a\nb\n");
    }

    #[test]
    fn 裏で書いても順番を保ち書き切ってから返る() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let out = Stdout::capture(buffer.clone());
        out.start_background(queue::CAPACITY);
        for n in 0..100 {
            out.write(format!("{n}\n"));
        }
        out.flush(Instant::now() + Duration::from_secs(5));
        let text = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        let expected: String = (0..100).map(|n| format!("{n}\n")).collect();
        assert_eq!(text, expected);
        assert_eq!(out.take_dropped(), 0);
    }

    #[test]
    fn キューが一杯なら捨てて数える() {
        // 書き手が止まっている状態を作るため、受け取る側を持ったまま読まない。
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let out = Stdout::capture(buffer);
        let (queue, receiver, _) = queue::channel::<String>(3);
        let _ = out.queue.set(queue);
        for n in 0..10 {
            out.write(format!("{n}\n"));
        }
        assert_eq!(out.take_dropped(), 7);
        drop(receiver);
    }
}
