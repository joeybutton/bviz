#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub metadata: Metadata,
    pub threads: Vec<ThreadTrack>,
    pub counters: Vec<CounterLane>,
    pub phase_markers: Vec<PhaseMarker>,
    pub span: TimeSpan,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Metadata {
    pub bazel_version: Option<String>,
    pub build_id: Option<String>,
    pub output_base: Option<String>,
    pub date: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSpan {
    pub start_us: i64,
    pub end_us: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadTrack {
    pub tid: i64,
    pub name: String,
    pub sort_index: i64,
    pub slices: Vec<Slice>,
    pub max_depth: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Slice {
    pub name: String,
    pub category: String,
    pub start_us: i64,
    pub dur_us: i64,
    pub depth: u32,
    pub args: SliceArgs,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SliceArgs {
    pub target: Option<String>,
    pub mnemonic: Option<String>,
    pub configuration: Option<String>,
    pub others: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CounterLane {
    pub name: String,
    pub series: Vec<CounterSeries>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CounterSeries {
    pub name: String,
    pub points: Vec<CounterPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CounterPoint {
    pub ts_us: i64,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseMarker {
    pub name: String,
    pub start_us: i64,
}
