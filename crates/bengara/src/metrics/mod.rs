//! メトリクス（`docs/logging.md` の「メトリクス」）。
//!
//! - 数はすべて Atomic で持ちます。ロックは使いません。
//! - 数えるのは本物の HTTP（`server.rs` の `dispatch`）だけです。テストの `app.handle` は数えません。
//! - `serve` が 60 秒ごとに数を取り出して 0 に戻し、`bengara::metrics` のイベントとして出します。

pub(crate) mod system;

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::logging::format::FieldValue;
use crate::logging::layer::METRICS_TARGET;

/// 出す間隔（固定）。
const INTERVAL: Duration = Duration::from_secs(60);

/// 応答時間の枠の上限（マイクロ秒）。最後に「上限なし」の枠がもう 1 つあります。
const BOUNDS_US: [u64; 18] = [
    100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000,
    1_000_000, 2_500_000, 5_000_000, 10_000_000, 30_000_000, 60_000_000,
];

/// 枠の数（上限なしを含む）。
const BUCKETS: usize = BOUNDS_US.len() + 1;

static REQUESTS: AtomicU64 = AtomicU64::new(0);
static STATUS_CLASS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
static STATUS_408: AtomicU64 = AtomicU64::new(0);
static STATUS_413: AtomicU64 = AtomicU64::new(0);
static STATUS_419: AtomicU64 = AtomicU64::new(0);
static STATUS_429: AtomicU64 = AtomicU64::new(0);
static HISTOGRAM: [AtomicU64; BUCKETS] = [const { AtomicU64::new(0) }; BUCKETS];
static MAX_US: AtomicU64 = AtomicU64::new(0);
static INFLIGHT: AtomicI64 = AtomicI64::new(0);
static PANICS: AtomicU64 = AtomicU64::new(0);

/// 応答時間の枠の番号。
fn bucket_of(micros: u64) -> usize {
    BOUNDS_US
        .iter()
        .position(|bound| micros <= *bound)
        .unwrap_or(BOUNDS_US.len())
}

/// 応答を 1 つ数える（`dispatch` が応答を返すとき）。
pub(crate) fn record(status: u16, elapsed: Duration) {
    REQUESTS.fetch_add(1, Ordering::Relaxed);
    if let Some(class) = (usize::from(status) / 100)
        .checked_sub(1)
        .filter(|c| *c < 5)
    {
        STATUS_CLASS[class].fetch_add(1, Ordering::Relaxed);
    }
    match status {
        408 => STATUS_408.fetch_add(1, Ordering::Relaxed),
        413 => STATUS_413.fetch_add(1, Ordering::Relaxed),
        419 => STATUS_419.fetch_add(1, Ordering::Relaxed),
        429 => STATUS_429.fetch_add(1, Ordering::Relaxed),
        _ => 0,
    };
    let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    HISTOGRAM[bucket_of(micros)].fetch_add(1, Ordering::Relaxed);
    MAX_US.fetch_max(micros, Ordering::Relaxed);
}

/// パニックを 1 つ数える（`run_stack`）。
pub(crate) fn record_panic() {
    PANICS.fetch_add(1, Ordering::Relaxed);
}

/// 処理中の数。作ると +1、落とすと −1。切断（future が落ちる）やパニックでも戻ります。
pub(crate) struct Inflight(&'static AtomicI64);

impl Inflight {
    pub(crate) fn enter() -> Self {
        Self::enter_on(&INFLIGHT)
    }

    fn enter_on(counter: &'static AtomicI64) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for Inflight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 区間の数の写し。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Counts {
    pub(crate) requests: u64,
    pub(crate) status_class: [u64; 5],
    pub(crate) status_408: u64,
    pub(crate) status_413: u64,
    pub(crate) status_419: u64,
    pub(crate) status_429: u64,
    pub(crate) buckets: [u64; BUCKETS],
    pub(crate) max_us: u64,
    pub(crate) inflight: u64,
    pub(crate) panics: u64,
}

/// 区間の数を取り出して 0 に戻す。区間の境目で 1 件ずれることは許します。
pub(crate) fn take() -> Counts {
    let swap = |counter: &AtomicU64| counter.swap(0, Ordering::Relaxed);
    Counts {
        requests: swap(&REQUESTS),
        status_class: std::array::from_fn(|i| swap(&STATUS_CLASS[i])),
        status_408: swap(&STATUS_408),
        status_413: swap(&STATUS_413),
        status_419: swap(&STATUS_419),
        status_429: swap(&STATUS_429),
        buckets: std::array::from_fn(|i| swap(&HISTOGRAM[i])),
        max_us: swap(&MAX_US),
        inflight: u64::try_from(INFLIGHT.load(Ordering::Relaxed)).unwrap_or(0),
        panics: swap(&PANICS),
    }
}

/// 分位（ミリ秒）。0 件なら `None`。
///
/// 小さい枠から数を足していき、「件数 × p」（切り上げ）に届いた枠の上限を出します。
/// 上限なしの枠なら、最大値を出します。`percent` は 50・95・99 のような百分率です。
pub(crate) fn percentile(buckets: &[u64; BUCKETS], percent: u64, max_us: u64) -> Option<f64> {
    let total: u64 = buckets.iter().sum();
    if total == 0 {
        return None;
    }
    // 小数の誤差を避けるため、整数で切り上げる。
    let rank = (u128::from(total) * u128::from(percent)).div_ceil(100);
    let mut seen: u128 = 0;
    for (index, count) in buckets.iter().enumerate() {
        seen += u128::from(*count);
        if seen >= rank {
            return Some(match BOUNDS_US.get(index) {
                Some(bound) => millis(*bound),
                None => millis(max_us),
            });
        }
    }
    Some(millis(max_us))
}

/// マイクロ秒をミリ秒にする（小数 3 桁まで。余計な 0 は付かない）。
fn millis(micros: u64) -> f64 {
    micros as f64 / 1_000.0
}

/// 60 秒ごとに出す 1 件。欄の名前と順は、SQLite の `metrics` 表の列と同じです。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Report {
    pub(crate) pid: u64,
    pub(crate) port: u64,
    pub(crate) uptime_seconds: u64,
    pub(crate) counts: Counts,
    pub(crate) websockets: Option<u64>,
    pub(crate) dropped: u64,
    pub(crate) system: system::Sample,
    pub(crate) cpu_percent: Option<f64>,
}

impl Report {
    fn has_requests(&self) -> bool {
        self.counts.buckets.iter().any(|count| *count > 0)
    }

    pub(crate) fn p50_ms(&self) -> Option<f64> {
        percentile(&self.counts.buckets, 50, self.counts.max_us)
    }

    pub(crate) fn p95_ms(&self) -> Option<f64> {
        percentile(&self.counts.buckets, 95, self.counts.max_us)
    }

    pub(crate) fn p99_ms(&self) -> Option<f64> {
        percentile(&self.counts.buckets, 99, self.counts.max_us)
    }

    pub(crate) fn max_ms(&self) -> Option<f64> {
        self.has_requests().then(|| millis(self.counts.max_us))
    }

    /// 欄を書く順に並べる。値が無いものは欄ごと入れません。
    ///
    /// 出すイベント（[`emit`](Self::emit)）は tracing のマクロで欄を並べるので、これを使えません。
    /// 2 つが同じ順になることを、テストで確かめます。
    #[cfg(test)]
    pub(crate) fn fields(&self) -> Vec<(&'static str, FieldValue)> {
        let c = &self.counts;
        let mut out: Vec<(&'static str, FieldValue)> = vec![
            ("pid", FieldValue::U64(self.pid)),
            ("port", FieldValue::U64(self.port)),
            ("uptime_seconds", FieldValue::U64(self.uptime_seconds)),
            ("requests", FieldValue::U64(c.requests)),
            ("status_1xx", FieldValue::U64(c.status_class[0])),
            ("status_2xx", FieldValue::U64(c.status_class[1])),
            ("status_3xx", FieldValue::U64(c.status_class[2])),
            ("status_4xx", FieldValue::U64(c.status_class[3])),
            ("status_5xx", FieldValue::U64(c.status_class[4])),
            ("status_408", FieldValue::U64(c.status_408)),
            ("status_413", FieldValue::U64(c.status_413)),
            ("status_419", FieldValue::U64(c.status_419)),
            ("status_429", FieldValue::U64(c.status_429)),
        ];
        let optional: [(&'static str, Option<FieldValue>); 4] = [
            ("p50_ms", self.p50_ms().map(FieldValue::F64)),
            ("p95_ms", self.p95_ms().map(FieldValue::F64)),
            ("p99_ms", self.p99_ms().map(FieldValue::F64)),
            ("max_ms", self.max_ms().map(FieldValue::F64)),
        ];
        out.extend(optional.into_iter().filter_map(|(k, v)| Some((k, v?))));
        out.push(("inflight", FieldValue::U64(c.inflight)));
        out.push(("panics", FieldValue::U64(c.panics)));
        if let Some(websockets) = self.websockets {
            out.push(("websockets", FieldValue::U64(websockets)));
        }
        out.push(("dropped", FieldValue::U64(self.dropped)));
        let tail: [(&'static str, Option<FieldValue>); 4] = [
            ("rss_bytes", self.system.rss_bytes.map(FieldValue::U64)),
            ("cpu_percent", self.cpu_percent.map(FieldValue::F64)),
            (
                "disk_free_bytes",
                self.system.disk_free_bytes.map(FieldValue::U64),
            ),
            (
                "disk_total_bytes",
                self.system.disk_total_bytes.map(FieldValue::U64),
            ),
        ];
        out.extend(tail.into_iter().filter_map(|(k, v)| Some((k, v?))));
        out
    }

    /// `bengara::metrics` のイベントとして出す。欄の順は [`fields`](Self::fields) と同じです。
    fn emit(&self) {
        let c = &self.counts;
        tracing::info!(
            target: METRICS_TARGET,
            pid = self.pid,
            port = self.port,
            uptime_seconds = self.uptime_seconds,
            requests = c.requests,
            status_1xx = c.status_class[0],
            status_2xx = c.status_class[1],
            status_3xx = c.status_class[2],
            status_4xx = c.status_class[3],
            status_5xx = c.status_class[4],
            status_408 = c.status_408,
            status_413 = c.status_413,
            status_419 = c.status_419,
            status_429 = c.status_429,
            p50_ms = self.p50_ms(),
            p95_ms = self.p95_ms(),
            p99_ms = self.p99_ms(),
            max_ms = self.max_ms(),
            inflight = c.inflight,
            panics = c.panics,
            websockets = self.websockets,
            dropped = self.dropped,
            rss_bytes = self.system.rss_bytes,
            cpu_percent = self.cpu_percent,
            disk_free_bytes = self.system.disk_free_bytes,
            disk_total_bytes = self.system.disk_total_bytes,
        );
    }
}

/// CPU の使用率（%）。コア数で割らない（100 を超えてよい）。小数 1 桁に丸めます。
pub(crate) fn cpu_percent(previous: (Instant, Duration), now: (Instant, Duration)) -> Option<f64> {
    let wall = now.0.checked_duration_since(previous.0)?.as_secs_f64();
    if wall <= 0.0 {
        return None;
    }
    let used = now.1.checked_sub(previous.1)?.as_secs_f64();
    Some((used / wall * 1_000.0).round() / 10.0)
}

/// `serve` の見回り。待ち受けを始めた後に立て、60 秒ごとに 1 件出します。
///
/// 最初に出すのは始めてから 60 秒後です。止めるときに最後の 1 件は出しません。
pub(crate) async fn watch(port: u16, sockets: Arc<crate::websocket::ServerState>) {
    let started = Instant::now();
    let storage = crate::paths::storage_root().to_path_buf();
    let sample = |storage: std::path::PathBuf| async move {
        tokio::task::spawn_blocking(move || system::sample(&storage))
            .await
            .unwrap_or_default()
    };
    // CPU 時間の前の値は、見回りを始めたときに取る（最初の 1 件から使用率を出せる）。
    let mut previous = sample(storage.clone())
        .await
        .cpu_time
        .map(|cpu| (Instant::now(), cpu));

    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + INTERVAL, INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let counts = take();
        let dropped = crate::logging::take_dropped();
        if !tracing::enabled!(target: METRICS_TARGET, tracing::Level::INFO) {
            // 出さないときも、数は取り出して 0 に戻す。
            continue;
        }
        let system = sample(storage.clone()).await;
        let now = system.cpu_time.map(|cpu| (Instant::now(), cpu));
        let cpu_percent = match (previous, now) {
            (Some(previous), Some(now)) => cpu_percent(previous, now),
            _ => None,
        };
        previous = now.or(previous);
        Report {
            pid: u64::from(std::process::id()),
            port: u64::from(port),
            uptime_seconds: started.elapsed().as_secs(),
            counts,
            websockets: cfg!(feature = "websocket").then(|| sockets.active() as u64),
            dropped,
            system,
            cpu_percent,
        }
        .emit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::format::logfmt;

    fn buckets(pairs: &[(usize, u64)]) -> [u64; BUCKETS] {
        let mut out = [0; BUCKETS];
        for (index, count) in pairs {
            out[*index] = *count;
        }
        out
    }

    #[test]
    fn 枠の番号() {
        assert_eq!(bucket_of(0), 0);
        assert_eq!(bucket_of(100), 0);
        assert_eq!(bucket_of(101), 1);
        assert_eq!(bucket_of(1_000), 3);
        assert_eq!(bucket_of(60_000_000), 17);
        assert_eq!(bucket_of(60_000_001), 18);
    }

    #[test]
    fn 分位は0件ならなし() {
        assert_eq!(percentile(&[0; BUCKETS], 50, 0), None);
    }

    #[test]
    fn 分位は1件ならその枠の上限() {
        let b = buckets(&[(3, 1)]);
        assert_eq!(percentile(&b, 50, 800), Some(1.0));
        assert_eq!(percentile(&b, 99, 800), Some(1.0));
    }

    #[test]
    fn 分位は枠の境目で切り上げる() {
        // 100 件：0.1 ms に 50 件、0.25 ms に 45 件、2.5 ms に 5 件。
        let b = buckets(&[(0, 50), (1, 45), (4, 5)]);
        assert_eq!(percentile(&b, 50, 2_400), Some(0.1));
        assert_eq!(percentile(&b, 95, 2_400), Some(0.25));
        assert_eq!(percentile(&b, 99, 2_400), Some(2.5));
        // 3 件の 50% は 2 件目（1.5 を切り上げ）。
        let b = buckets(&[(0, 1), (2, 1), (5, 1)]);
        assert_eq!(percentile(&b, 50, 4_000), Some(0.5));
    }

    #[test]
    fn 上限なしの枠なら最大値() {
        let b = buckets(&[(0, 1), (18, 1)]);
        assert_eq!(percentile(&b, 99, 61_234_567), Some(61_234.567));
    }

    #[test]
    fn 取り出すと0に戻る() {
        // ほかのテストも数えるので、差で確かめる。
        let _ = take();
        record(200, Duration::from_micros(150));
        record(419, Duration::from_millis(3));
        record(503, Duration::from_secs(70));
        let counts = take();
        assert!(counts.requests >= 3);
        assert!(counts.status_419 >= 1);
        assert!(counts.max_us >= 70_000_000);
        let again = take();
        assert_eq!(again.status_419, 0);
        assert_eq!(again.max_us, 0);
    }

    #[test]
    fn 処理中の数はパニックでも戻る() {
        static COUNTER: AtomicI64 = AtomicI64::new(0);
        let result = std::panic::catch_unwind(|| {
            let _guard = Inflight::enter_on(&COUNTER);
            assert_eq!(COUNTER.load(Ordering::Relaxed), 1);
            panic!("わざと");
        });
        assert!(result.is_err());
        assert_eq!(COUNTER.load(Ordering::Relaxed), 0);
    }

    fn report() -> Report {
        Report {
            pid: 4123,
            port: 8001,
            uptime_seconds: 60,
            counts: Counts {
                requests: 5320,
                status_class: [0, 5200, 80, 40, 0],
                status_408: 0,
                status_413: 0,
                status_419: 1,
                status_429: 3,
                buckets: buckets(&[(3, 3000), (5, 2200), (6, 70), (7, 50)]),
                max_us: 31_204,
                inflight: 2,
                panics: 0,
            },
            websockets: Some(14),
            dropped: 0,
            system: system::Sample {
                rss_bytes: Some(10_289_152),
                cpu_time: None,
                disk_free_bytes: Some(44_567_883_776),
                disk_total_bytes: Some(105_088_212_992),
            },
            cpu_percent: Some(3.2),
        }
    }

    #[test]
    fn 写しからlogfmtの行を組み立てる() {
        let line = logfmt(Some("2026-11-26T02:52:00.000Z"), &report().fields());
        assert_eq!(
            line,
            "time=2026-11-26T02:52:00.000Z pid=4123 port=8001 uptime_seconds=60 requests=5320 status_1xx=0 status_2xx=5200 status_3xx=80 status_4xx=40 status_5xx=0 status_408=0 status_413=0 status_419=1 status_429=3 p50_ms=1 p95_ms=5 p99_ms=10 max_ms=31.204 inflight=2 panics=0 websockets=14 dropped=0 rss_bytes=10289152 cpu_percent=3.2 disk_free_bytes=44567883776 disk_total_bytes=105088212992"
        );
    }

    #[test]
    fn 無い値の鍵は出さない() {
        let mut r = report();
        r.counts.buckets = [0; BUCKETS];
        r.counts.max_us = 0;
        r.websockets = None;
        r.system = system::Sample::default();
        r.cpu_percent = None;
        let line = logfmt(None, &r.fields());
        for key in [
            "p50_ms",
            "p95_ms",
            "p99_ms",
            "max_ms",
            "websockets",
            "rss_bytes",
            "cpu_percent",
            "disk_free_bytes",
            "disk_total_bytes",
        ] {
            assert!(!line.contains(&format!("{key}=")), "{key}: {line}");
        }
        assert!(line.contains("dropped=0"), "{line}");
    }

    #[test]
    fn 出すイベントの欄は写しと同じ順() {
        use crate::logging::layer::Collect;
        use tracing_subscriber::prelude::*;

        struct Grab(std::sync::Arc<std::sync::Mutex<Vec<(&'static str, FieldValue)>>>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Grab {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let mut collect = Collect::default();
                event.record(&mut collect);
                *self.0.lock().unwrap() = collect.fields;
            }
        }
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Grab(seen.clone()));
        let r = report();
        tracing::subscriber::with_default(subscriber, || r.emit());
        assert_eq!(*seen.lock().unwrap(), r.fields());
    }

    #[test]
    fn cpuの使用率() {
        let start = Instant::now();
        let later = start + Duration::from_secs(60);
        assert_eq!(
            cpu_percent(
                (start, Duration::from_secs(1)),
                (later, Duration::from_millis(2_920))
            ),
            Some(3.2)
        );
        // コア数で割らない。
        assert_eq!(
            cpu_percent((start, Duration::ZERO), (later, Duration::from_secs(120))),
            Some(200.0)
        );
        assert_eq!(
            cpu_percent((later, Duration::ZERO), (start, Duration::ZERO)),
            None
        );
    }
}
