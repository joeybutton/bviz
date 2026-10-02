use std::ops::Range;

use bviz_parallel::workers;
use bviz_trace_profile::{
    FetchSide, LaneClass, Profile, Repository, Slice, fetch_side, parks_thread,
};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use rayon::slice::ParallelSliceMut;

use crate::flame::run_sharded;

/// What a lane with a span open is doing, which decides who an instant goes
/// to: it is shared among the lanes of the foremost tier any lane is in, and the
/// lanes of the tiers behind it get none of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    /// A garbage-collector lane's span: a stop-the-world pause, stalling every
    /// other lane's open span.
    Pause,
    /// The lane's innermost open span is work of its own.
    Working,
    /// The lane's innermost open span holds it parked (see [`parks_thread`]
    /// and [`Standing::Hosting`]): what it waits on is other lanes' work, so it
    /// is given only the time none of them works.
    Parked,
}

impl Tier {
    const FOREMOST: Tier = Tier::Pause;

    /// The tier next behind this one.
    fn behind(self) -> Option<Tier> {
        match self {
            Tier::Pause => Some(Tier::Working),
            Tier::Working => Some(Tier::Parked),
            Tier::Parked => None,
        }
    }

    /// Every tier, foremost first.
    fn foremost_first() -> impl Iterator<Item = Tier> {
        std::iter::successors(Some(Tier::FOREMOST), |tier| tier.behind())
    }
}

/// One `T` per [`Tier`], reached through [`of`](Self::of) and
/// [`of_mut`](Self::of_mut), whose exhaustive matches bind the slots to the
/// variants.
#[derive(Clone, Copy, Default)]
struct ByTier<T> {
    pause: T,
    working: T,
    parked: T,
}

impl<T> ByTier<T> {
    fn of(&self, tier: Tier) -> &T {
        match tier {
            Tier::Pause => &self.pause,
            Tier::Working => &self.working,
            Tier::Parked => &self.parked,
        }
    }

    fn of_mut(&mut self, tier: Tier) -> &mut T {
        match tier {
            Tier::Pause => &mut self.pause,
            Tier::Working => &mut self.working,
            Tier::Parked => &mut self.parked,
        }
    }

    fn map<U>(self, mut f: impl FnMut(T) -> U) -> ByTier<U> {
        ByTier {
            pause: f(self.pause),
            working: f(self.working),
            parked: f(self.parked),
        }
    }
}

/// Fractional bits of a [`Fixed`] reading. A microsecond shared `n` ways
/// advances a clock by a fraction of one, and the division truncates: every
/// change of a clock's rate leaves its reading under one unit further short of
/// the exact share, never ahead of it. The shortfall thus grows with the
/// number of changes behind a reading: a whole microsecond of it takes
/// 2^`FRACTION_BITS` of them.
const FRACTION_BITS: u32 = 20;

/// A clock reading in units of 2^-[`FRACTION_BITS`] µs.
type Fixed = i64;

/// The longest time a [`Fixed`] reading holds, some hundred days. A clock run
/// for longer reads as this.
const MAX_READING_US: i64 = Fixed::MAX >> FRACTION_BITS;

/// The whole microseconds nearest a reading.
fn whole_us(reading: Fixed) -> i64 {
    const HALF: Fixed = 1 << (FRACTION_BITS - 1);
    reading.saturating_add(HALF) >> FRACTION_BITS
}

/// How many of the ascending `xs` are at or before `t`, searched outward from
/// `hint`: a query at or ahead of the one that produced `hint` costs the
/// logarithm of its distance from it. Any hint gives the same answer.
fn seek(xs: &[i64], hint: usize, t: i64) -> usize {
    let hint = hint.min(xs.len());
    if hint > 0 && xs[hint - 1] > t {
        return xs[..hint - 1].partition_point(|&x| x <= t);
    }
    // Everything before `lo` is at or before `t`; the probe doubles its reach
    // until it lands past `t` or off the end.
    let mut lo = hint;
    let mut step = 1;
    while lo + step <= xs.len() && xs[lo + step - 1] <= t {
        lo += step;
        step *= 2;
    }
    let hi = (lo + step - 1).min(xs.len());
    lo + xs[lo..hi].partition_point(|&x| x <= t)
}

/// One tier's clock: the elapsed time a single lane of the tier has been given
/// by each instant. It advances at `1 / sharers` while the tier is foremost with
/// `sharers` lanes in it and stands otherwise, so it is monotone, and a lane
/// that is in the tier from one instant to another is given the difference of
/// the two readings. The three vectors are index-parallel, one entry per change
/// of rate.
struct ShareClock {
    at_us: Vec<i64>,
    reading: Vec<Fixed>,
    /// Zero while the clock stands.
    sharers: Vec<u32>,
}

impl ShareClock {
    fn with_capacity(changes: usize) -> ShareClock {
        ShareClock {
            at_us: Vec::with_capacity(changes),
            reading: Vec::with_capacity(changes),
            sharers: Vec::with_capacity(changes),
        }
    }

    /// Runs the clock at `1 / sharers` from `at_us` on, which must not precede
    /// the last change.
    fn change(&mut self, at_us: i64, sharers: u32) {
        let reading = self.reading_at(at_us, &mut self.at_us.len());
        self.at_us.push(at_us);
        self.reading.push(reading);
        self.sharers.push(sharers);
    }

    /// The reading at `t_us`. `hint` is a [`seek`] hint, left at this query's
    /// answer.
    fn reading_at(&self, t_us: i64, hint: &mut usize) -> Fixed {
        *hint = seek(&self.at_us, *hint, t_us);
        let Some(last) = hint.checked_sub(1) else {
            return 0;
        };
        let sharers = self.sharers[last];
        if sharers == 0 {
            return self.reading[last];
        }
        let elapsed_us = t_us.saturating_sub(self.at_us[last]).min(MAX_READING_US);
        let advanced = (elapsed_us << FRACTION_BITS) / Fixed::from(sharers);
        self.reading[last].saturating_add(advanced)
    }
}

/// A stretch over which a lane has a span open and stays in one tier.
#[derive(Clone, Copy)]
struct OpenRun {
    start_us: i64,
    end_us: i64,
    tier: Tier,
}

/// What a span puts its lane in while it is the lane's innermost open one.
#[derive(Clone, Copy)]
enum Standing<'p> {
    In(Tier),
    /// The host of this repository's fetch (see [`FetchSide::Host`]):
    /// [`Tier::Parked`] while a [`WorkerSpan`] of the repository is open on
    /// another lane, and [`Tier::Working`] for the rest of it.
    Hosting(Repository<'p>),
}

/// What a span of one Bazel `cat` puts its lane in, as far as the category
/// says.
#[derive(Clone, Copy)]
enum CategoryStanding {
    Parks,
    Works,
    /// Left to the span's name: see [`FetchSide::repository`].
    Fetches(FetchSide),
}

impl CategoryStanding {
    fn of(category: &str) -> CategoryStanding {
        if parks_thread(category) {
            return CategoryStanding::Parks;
        }
        match fetch_side(category) {
            Some(side) => CategoryStanding::Fetches(side),
            None => CategoryStanding::Works,
        }
    }

    /// The standing of each category `profile` numbers, in the order of
    /// [`Profile::categories`].
    fn numbered(profile: &Profile) -> Vec<CategoryStanding> {
        profile
            .categories()
            .map(|category| CategoryStanding::of(profile.str(category)))
            .collect()
    }
}

/// A span open on `lane` as the worker of `repository`'s fetch (see
/// [`FetchSide::Worker`]).
#[derive(Clone, Copy)]
struct WorkerSpan<'p> {
    repository: Repository<'p>,
    start_us: i64,
    end_us: i64,
    /// The lane's place in [`Profile::threads`].
    lane: usize,
}

/// A profile's [`WorkerSpan`]s, ordered by repository and then by start.
struct FetchWorkers<'p>(Vec<WorkerSpan<'p>>);

impl<'p> FetchWorkers<'p> {
    fn new(mut spans: Vec<WorkerSpan<'p>>) -> FetchWorkers<'p> {
        spans.sort_unstable_by_key(|span| (span.repository, span.start_us));
        FetchWorkers(spans)
    }

    /// The worker spans every one of `shards` read.
    fn gathered(shards: &[RunShard<'p>]) -> FetchWorkers<'p> {
        let span_count = shards.iter().map(|shard| shard.workers.len()).sum();
        let mut spans = Vec::with_capacity(span_count);
        for shard in shards {
            spans.extend_from_slice(&shard.workers);
        }
        FetchWorkers::new(spans)
    }

    /// Where the spans of `repository` are.
    fn of(&self, repository: Repository<'p>) -> Range<usize> {
        let from = self.0.partition_point(|span| span.repository < repository);
        let len = self.0[from..].partition_point(|span| span.repository == repository);
        from..from + len
    }

    /// Reports, in order, the abutting stretches that make up `run`, which a
    /// host has open on `host_lane` waiting on the spans at `workers`, each
    /// in the tier the host is in over it.
    fn split(
        &self,
        host_lane: usize,
        workers: Range<usize>,
        run: OpenRun,
        mut stretch: impl FnMut(OpenRun),
    ) {
        let mut stretch = |start_us, end_us, tier| {
            stretch(OpenRun {
                start_us,
                end_us,
                tier,
            })
        };
        let mut at_us = run.start_us;
        for worker in &self.0[workers] {
            if at_us >= run.end_us || worker.start_us >= run.end_us {
                break;
            }
            if worker.lane == host_lane || worker.end_us <= at_us {
                continue;
            }
            let parked_from_us = worker.start_us.max(at_us);
            if parked_from_us > at_us {
                stretch(at_us, parked_from_us, Tier::Working);
            }
            at_us = worker.end_us.min(run.end_us);
            stretch(parked_from_us, at_us, Tier::Parked);
        }
        if at_us < run.end_us {
            stretch(at_us, run.end_us, Tier::Working);
        }
    }
}

/// A run a host has open, known by `H`: the [`Repository`] it fetches as
/// swept, and where that repository's spans are in the profile's
/// [`FetchWorkers`] once those are gathered, so that each host's are sought
/// once.
struct HostRun<H> {
    /// Its place in [`SweptRuns::swept`].
    run: usize,
    waits_on: H,
}

/// A lane's [`OpenRun`]s under construction.
struct RunSweep<'a, 'p> {
    /// The end and standing of each span open at `now_us`, in the order they
    /// began.
    open: &'a mut Vec<(i64, Standing<'p>)>,
    out: &'a mut Vec<OpenRun>,
    hosts: &'a mut Vec<HostRun<Repository<'p>>>,
    /// Where this lane's runs begin in `out`, which holds other lanes' ahead
    /// of them.
    first: usize,
    now_us: i64,
}

impl<'p> RunSweep<'_, 'p> {
    /// Emits the lane's runs from `now_us` up to `until_us`, dropping the spans
    /// that end by then. The innermost open span is the latest-begun one still
    /// open, which also orders spans that overlap without nesting.
    fn advance(&mut self, until_us: i64) {
        while let Some(&(end_us, standing)) = self.open.last() {
            if end_us > self.now_us {
                let upto_us = end_us.min(until_us);
                if upto_us > self.now_us {
                    self.emit(upto_us, standing);
                }
                if end_us > until_us {
                    break;
                }
            }
            self.open.pop();
        }
    }

    fn emit(&mut self, end_us: i64, standing: Standing<'p>) {
        let (tier, hosted) = match standing {
            Standing::In(tier) => (tier, None),
            Standing::Hosting(repository) => (Tier::Working, Some(repository)),
        };
        let follows_host = self
            .hosts
            .last()
            .is_some_and(|host| host.run + 1 == self.out.len());
        match self.out[self.first..].last_mut() {
            Some(last)
                if hosted.is_none()
                    && !follows_host
                    && last.end_us == self.now_us
                    && last.tier == tier =>
            {
                last.end_us = end_us
            }
            _ => {
                if let Some(repository) = hosted {
                    self.hosts.push(HostRun {
                        run: self.out.len(),
                        waits_on: repository,
                    });
                }
                self.out.push(OpenRun {
                    start_us: self.now_us,
                    end_us,
                    tier,
                });
            }
        }
        self.now_us = end_us;
    }
}

/// Appends to `out` the [`OpenRun`]s of the lane holding `slices`, ascending
/// and with a run that continues the last in its tier merged into it unless
/// either is a host's, and to `hosts` those a host has open. `standing_of`
/// gives the standing of the slice at an index; `open` is scratch.
fn open_runs<'p>(
    slices: &[Slice],
    mut standing_of: impl FnMut(usize) -> Standing<'p>,
    open: &mut Vec<(i64, Standing<'p>)>,
    out: &mut Vec<OpenRun>,
    hosts: &mut Vec<HostRun<Repository<'p>>>,
) {
    open.clear();
    let first = out.len();
    let mut sweep = RunSweep {
        open,
        out,
        hosts,
        first,
        now_us: i64::MIN,
    };
    for (i, slice) in slices.iter().enumerate() {
        sweep.advance(slice.start_us);
        sweep.now_us = slice.start_us;
        // A slice of zero duration is open at no instant.
        if slice.dur_us > 0 {
            sweep.open.push((slice.end_us(), standing_of(i)));
        }
    }
    sweep.advance(i64::MAX);
}

/// The runs of some lanes as swept, a host's standing as working throughout:
/// [`for_each_run`](Self::for_each_run) gives a lane's with its hosts parked.
struct SweptRuns<H> {
    swept: Vec<OpenRun>,
    /// Each lane read, with the range of `swept` that is its own.
    lanes: Vec<(usize, Range<usize>)>,
    /// Ascending by run.
    hosts: Vec<HostRun<H>>,
}

impl<H> Default for SweptRuns<H> {
    fn default() -> Self {
        SweptRuns {
            swept: Vec::new(),
            lanes: Vec::new(),
            hosts: Vec::new(),
        }
    }
}

impl<'p> SweptRuns<Repository<'p>> {
    fn waiting_on(self, workers: &FetchWorkers<'p>) -> SweptRuns<Range<usize>> {
        let hosts = self.hosts.into_iter().map(|host| HostRun {
            run: host.run,
            waits_on: workers.of(host.waits_on),
        });
        SweptRuns {
            swept: self.swept,
            lanes: self.lanes,
            hosts: hosts.collect(),
        }
    }
}

impl SweptRuns<Range<usize>> {
    fn hosts_in(&self, runs: &Range<usize>) -> &[HostRun<Range<usize>>] {
        let from = self.hosts.partition_point(|host| host.run < runs.start);
        let len = self.hosts[from..].partition_point(|host| host.run < runs.end);
        &self.hosts[from..from + len]
    }

    /// Reports the runs of the lane at `at` in `lanes`, ascending, each
    /// host's split where `workers` park it.
    fn for_each_run(&self, at: usize, workers: &FetchWorkers, mut run: impl FnMut(OpenRun)) {
        let (lane, range) = &self.lanes[at];
        let mut from = range.start;
        for host in self.hosts_in(range) {
            self.swept[from..host.run]
                .iter()
                .copied()
                .for_each(&mut run);
            let waits_on = host.waits_on.clone();
            workers.split(*lane, waits_on, self.swept[host.run], &mut run);
            from = host.run + 1;
        }
        self.swept[from..range.end].iter().copied().for_each(run);
    }

    /// How many runs [`for_each_run`](Self::for_each_run) reports of the lane
    /// at `at`.
    fn run_count(&self, at: usize, workers: &FetchWorkers) -> usize {
        let (lane, range) = &self.lanes[at];
        let hosts = self.hosts_in(range);
        let mut stretches = 0;
        for host in hosts {
            let waits_on = host.waits_on.clone();
            workers.split(*lane, waits_on, self.swept[host.run], |_| stretches += 1);
        }
        range.len() - hosts.len() + stretches
    }
}

/// What one worker read of its lanes.
#[derive(Default)]
struct RunShard<'p> {
    runs: SweptRuns<Repository<'p>>,
    workers: Vec<WorkerSpan<'p>>,
    /// [`open_runs`]' scratch, kept from lane to lane.
    open: Vec<(i64, Standing<'p>)>,
}

impl<'p> RunShard<'p> {
    /// Reads the lane at `lane` in [`Profile::threads`], `standing_by_category`
    /// being the profile's [`CategoryStanding::numbered`].
    fn sweep(
        &mut self,
        profile: &'p Profile,
        standing_by_category: &[CategoryStanding],
        lane: usize,
    ) {
        let thread = &profile.threads()[lane];
        let slices = profile.slices(thread);
        let details = profile.details(thread);
        let from = self.runs.swept.len();
        let fetch_workers = &mut self.workers;
        let standing_of = |at: usize| {
            if thread.lane_class == LaneClass::Gc {
                return Standing::In(Tier::Pause);
            }
            let slice = &slices[at];
            let by_category = match slice.category {
                Some(id) => standing_by_category[id.position()],
                None => CategoryStanding::of(profile.str(details[at].category)),
            };
            match by_category {
                CategoryStanding::Parks => Standing::In(Tier::Parked),
                CategoryStanding::Works => Standing::In(Tier::Working),
                CategoryStanding::Fetches(side) => {
                    fetch_standing(side, profile.str(slice.name), slice, lane, fetch_workers)
                }
            }
        };
        if thread.lane_class != LaneClass::CriticalPath {
            open_runs(
                slices,
                standing_of,
                &mut self.open,
                &mut self.runs.swept,
                &mut self.runs.hosts,
            );
        }
        self.runs.lanes.push((lane, from..self.runs.swept.len()));
    }
}

/// The standing of `slice`, a span of `side` of a fetch named `name` and open
/// on `lane`, which joins `workers` where it is a repository's worker.
fn fetch_standing<'p>(
    side: FetchSide,
    name: &'p str,
    slice: &Slice,
    lane: usize,
    workers: &mut Vec<WorkerSpan<'p>>,
) -> Standing<'p> {
    match (side, side.repository(name)) {
        (FetchSide::Host, Some(repository)) => Standing::Hosting(repository),
        (FetchSide::Worker, Some(repository)) => {
            workers.push(WorkerSpan {
                repository,
                start_us: slice.start_us,
                end_us: slice.end_us(),
                lane,
            });
            Standing::In(Tier::Working)
        }
        (_, None) => Standing::In(Tier::Working),
    }
}

/// Where a lane's runs are among the shards' [`SweptRuns`].
#[derive(Clone, Copy, Default)]
struct LaneHome {
    shard: usize,
    /// The lane's place in the shard's `lanes`.
    at: usize,
    /// How many runs [`SweptRuns::for_each_run`] reports of the lane.
    run_count: usize,
}

/// Each lane's home, in the order of [`Profile::threads`], which holds
/// `lane_count` lanes.
fn lane_homes(
    shards: &[SweptRuns<Range<usize>>],
    fetch_workers: &FetchWorkers,
    lane_count: usize,
) -> Vec<LaneHome> {
    let mut homes = vec![LaneHome::default(); lane_count];
    for (s, shard) in shards.iter().enumerate() {
        for (at, (lane, _)) in shard.lanes.iter().enumerate() {
            homes[*lane] = LaneHome {
                shard: s,
                at,
                run_count: shard.run_count(at, fetch_workers),
            };
        }
    }
    homes
}

/// A lane entering or leaving a tier.
#[derive(Clone, Copy)]
struct Edge {
    at_us: i64,
    tier: Tier,
    enters: bool,
}

/// Walks `edges`, ascending by time, and reports each change in how many lanes
/// share an instant under a tier's clock, as `(tier, time, sharers)`. Edges at
/// one instant apply together, so a lane handing over to another reports
/// nothing.
fn for_each_change(edges: &[Edge], mut change: impl FnMut(Tier, i64, u32)) {
    let mut lanes_in = ByTier::<u32>::default();
    let mut sharers = ByTier::<u32>::default();
    let mut i = 0;
    while i < edges.len() {
        let at_us = edges[i].at_us;
        while i < edges.len() && edges[i].at_us == at_us {
            let n = lanes_in.of_mut(edges[i].tier);
            if edges[i].enters {
                *n += 1;
            } else {
                *n -= 1;
            }
            i += 1;
        }
        let foremost = Tier::foremost_first().find(|&tier| *lanes_in.of(tier) > 0);
        for tier in Tier::foremost_first() {
            let now = if foremost == Some(tier) {
                *lanes_in.of(tier)
            } else {
                0
            };
            let sharing = sharers.of_mut(tier);
            if now != *sharing {
                *sharing = now;
                change(tier, at_us, now);
            }
        }
    }
}

/// The [`Edge`]s of every run of the lanes at `homes`, ascending by time.
fn edges_by_time(
    shards: &[SweptRuns<Range<usize>>],
    fetch_workers: &FetchWorkers,
    homes: &[LaneHome],
) -> Vec<Edge> {
    let run_count: usize = homes.iter().map(|home| home.run_count).sum();
    let mut edges: Vec<Edge> = Vec::with_capacity(2 * run_count);
    for shard in shards {
        for at in 0..shard.lanes.len() {
            shard.for_each_run(at, fetch_workers, |run| {
                edges.push(Edge {
                    at_us: run.start_us,
                    tier: run.tier,
                    enters: true,
                });
                edges.push(Edge {
                    at_us: run.end_us,
                    tier: run.tier,
                    enters: false,
                });
            });
        }
    }
    workers().install(|| edges.par_sort_unstable_by_key(|e| e.at_us));
    edges
}

/// Each tier's clock over `edges`, which are ascending by time.
fn tier_clocks(edges: &[Edge]) -> ByTier<ShareClock> {
    let mut changes = ByTier::<usize>::default();
    for_each_change(edges, |tier, _, _| *changes.of_mut(tier) += 1);
    let mut clocks = changes.map(ShareClock::with_capacity);
    for_each_change(edges, |tier, at_us, sharers| {
        clocks.of_mut(tier).change(at_us, sharers)
    });
    clocks
}

/// A lane's own clock over its [`OpenRun`]s, as two index-parallel vectors with
/// the runs' starts apart so that finding the run an instant falls in reads
/// only those.
struct LaneRuns {
    start_us: Vec<i64>,
    runs: Vec<Run>,
}

struct Run {
    end_us: i64,
    tier: Tier,
    /// What the lane's reading inside the run exceeds its tier's clock by.
    origin: Fixed,
}

/// Where the last query landed, as [`seek`] hints for the next.
#[derive(Clone, Copy, Default)]
struct Hints {
    run: usize,
    clock: ByTier<usize>,
}

impl LaneRuns {
    /// The clock of a lane over the `run_count` runs `for_each_run` reports,
    /// which are ascending.
    fn new(
        run_count: usize,
        clocks: &ByTier<ShareClock>,
        for_each_run: impl FnOnce(&mut dyn FnMut(OpenRun)),
    ) -> LaneRuns {
        let mut start_us = Vec::with_capacity(run_count);
        let mut runs = Vec::with_capacity(run_count);
        let mut hints = ByTier::<usize>::default();
        let mut given: Fixed = 0;
        for_each_run(&mut |open| {
            let clock = clocks.of(open.tier);
            let hint = hints.of_mut(open.tier);
            let at_start = clock.reading_at(open.start_us, hint);
            let at_end = clock.reading_at(open.end_us, hint);
            start_us.push(open.start_us);
            runs.push(Run {
                end_us: open.end_us,
                tier: open.tier,
                origin: given - at_start,
            });
            given = given.saturating_add(at_end - at_start);
        });
        LaneRuns { start_us, runs }
    }

    /// The elapsed time the lane has been given by `t_us`.
    fn reading_at(&self, clocks: &ByTier<ShareClock>, t_us: i64, hints: &mut Hints) -> Fixed {
        hints.run = seek(&self.start_us, hints.run, t_us);
        let Some(run) = hints.run.checked_sub(1).map(|i| &self.runs[i]) else {
            return 0;
        };
        let reading = clocks
            .of(run.tier)
            .reading_at(t_us.min(run.end_us), hints.clock.of_mut(run.tier));
        run.origin.saturating_add(reading)
    }
}

enum Lane {
    /// The critical-path lane: its spans replay actions that ran on other
    /// lanes, one after another, so it neither takes a share of theirs nor
    /// shares its own, and a span of it weighs its plain duration.
    Replay,
    Sharing(LaneRuns),
}

/// Each lane's clock over the runs `shards` hold of it, in the order of
/// [`Profile::threads`].
fn lane_clocks(
    profile: &Profile,
    shards: &[SweptRuns<Range<usize>>],
    fetch_workers: &FetchWorkers,
    homes: &[LaneHome],
    clocks: &ByTier<ShareClock>,
) -> Vec<Lane> {
    let threads = profile.threads();
    workers().install(|| {
        (0..threads.len())
            .into_par_iter()
            .map(|i| match threads[i].lane_class {
                LaneClass::CriticalPath => Lane::Replay,
                _ => {
                    let home = homes[i];
                    let shard = &shards[home.shard];
                    Lane::Sharing(LaneRuns::new(home.run_count, clocks, |run| {
                        shard.for_each_run(home.at, fetch_workers, run)
                    }))
                }
            })
            .collect()
    })
}

/// A profile's elapsed time split among its lanes. Each instant goes in equal
/// parts to the lanes of the foremost [`Tier`] any lane is in, and to no lane
/// while none has a span open, so over any stretch the lanes' shares sum,
/// before rounding, to the time some lane had a span open in it. Which lanes
/// share is a property of the profile alone, whatever subset of them a view goes
/// on to show.
///
/// A share is elapsed microseconds. Each lane's clock is rounded to whole
/// microseconds as a function of time and a stretch weighs the difference of
/// its two ends, so the weights of stretches that abut sum to the weight of the
/// stretch they make up, exactly, and a stretch never outweighs one holding it.
/// Each lane rounding on its own, the lanes' shares of a stretch sum to its
/// open time to within a microsecond per lane.
pub(crate) struct WallShare {
    clocks: ByTier<ShareClock>,
    /// One per [`Profile::threads`] entry.
    lanes: Vec<Lane>,
}

impl WallShare {
    /// Builds the profile's shares, reading its lanes across `fan_out` workers.
    pub(crate) fn new(profile: &Profile, fan_out: usize) -> WallShare {
        let lane_count = profile.threads().len();
        let standing_by_category = &CategoryStanding::numbered(profile);
        let shards = run_sharded(fan_out, lane_count, |shard: &mut RunShard, lane| {
            shard.sweep(profile, standing_by_category, lane)
        });
        let fetch_workers = &FetchWorkers::gathered(&shards);
        let shards: Vec<SweptRuns<Range<usize>>> = shards
            .into_iter()
            .map(|shard| shard.runs.waiting_on(fetch_workers))
            .collect();
        let homes = lane_homes(&shards, fetch_workers, lane_count);
        let clocks = tier_clocks(&edges_by_time(&shards, fetch_workers, &homes));
        let lanes = lane_clocks(profile, &shards, fetch_workers, &homes, &clocks);
        WallShare { clocks, lanes }
    }

    /// The share of the lane at `lane` in [`Profile::threads`].
    pub(crate) fn lane(&self, lane: usize) -> LaneShare<'_> {
        LaneShare {
            clocks: &self.clocks,
            lane: &self.lanes[lane],
            hints: Hints::default(),
        }
    }
}

/// One lane's share of elapsed time, for weighing stretches of it. It keeps
/// where the last stretch began, so weighing stretches in the order they begin
/// — the order a lane's slices come in — finds each next to the last.
pub(crate) struct LaneShare<'a> {
    clocks: &'a ByTier<ShareClock>,
    lane: &'a Lane,
    hints: Hints,
}

impl LaneShare<'_> {
    /// The elapsed microseconds the lane is given over `[start_us, end_us)`.
    pub(crate) fn between(&mut self, start_us: i64, end_us: i64) -> i64 {
        if end_us <= start_us {
            return 0;
        }
        let runs = match self.lane {
            Lane::Replay => return end_us.saturating_sub(start_us),
            Lane::Sharing(runs) => runs,
        };
        let from = runs.reading_at(self.clocks, start_us, &mut self.hints);
        // The end lies ahead of the start, so it is sought from there; the
        // kept hints stay at the start, which the next stretch begins near.
        let mut ahead = self.hints;
        let to = runs.reading_at(self.clocks, end_us, &mut ahead);
        whole_us(to) - whole_us(from)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fastrand::Rng;

    use super::*;
    use crate::test_profile::{
        TestSlice, fetch_host, fetch_worker, profile, random_profile, slice, track, wait,
    };

    /// A slice of work of its lane's own.
    fn work(start_us: i64, dur_us: i64) -> TestSlice {
        slice("work", "generic", start_us, dur_us)
    }

    fn pause(start_us: i64, dur_us: i64) -> TestSlice {
        slice("minor GC", "gc notification", start_us, dur_us)
    }

    /// Each lane's share of `[start_us, end_us)`, in track order.
    fn shares(p: &Arc<Profile>, start_us: i64, end_us: i64) -> Vec<i64> {
        let share = WallShare::new(p, 2);
        (0..p.threads().len())
            .map(|i| share.lane(i).between(start_us, end_us))
            .collect()
    }

    #[test]
    fn seek_counts_the_entries_at_or_before_an_instant_from_any_hint() {
        let xs = [2, 2, 5, 9, 9, 9, 14, 20];
        for len in 0..=xs.len() {
            let xs = &xs[..len];
            for t in 0..22 {
                let expected = xs.partition_point(|&x| x <= t);
                for hint in 0..=len + 2 {
                    assert_eq!(seek(xs, hint, t), expected, "{xs:?} hint {hint} t {t}");
                }
            }
        }
    }

    #[test]
    fn lanes_working_together_split_the_time_they_overlap() {
        let p = profile(vec![
            track(1, LaneClass::Execution, vec![work(0, 100)]),
            track(2, LaneClass::Execution, vec![work(50, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 150), [75, 75]);
        assert_eq!(shares(&p, 0, 50), [50, 0]);
        assert_eq!(shares(&p, 50, 100), [25, 25]);
        // Nothing is open past the last span, and nothing is given.
        assert_eq!(shares(&p, 150, 400), [0, 0]);
    }

    #[test]
    fn a_parked_lane_is_given_only_the_time_no_lane_works() {
        let p = profile(vec![
            track(1, LaneClass::Main, vec![wait(0, 300)]),
            track(2, LaneClass::Execution, vec![work(100, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 300), [200, 100]);
        assert_eq!(shares(&p, 100, 200), [0, 100]);
    }

    #[test]
    fn work_nested_in_a_parking_span_makes_its_lane_work() {
        // The main thread works from 50 to 100 inside its wait, alongside the
        // other lane; from 100 to 150 it is parked while the other still works.
        let p = profile(vec![
            track(1, LaneClass::Main, vec![wait(0, 300), work(50, 50)]),
            track(2, LaneClass::Execution, vec![work(50, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 50), [50, 0]);
        assert_eq!(shares(&p, 50, 100), [25, 25]);
        assert_eq!(shares(&p, 100, 150), [0, 50]);
        assert_eq!(shares(&p, 150, 300), [150, 0]);
        assert_eq!(shares(&p, 0, 300), [225, 75]);
    }

    #[test]
    fn parked_lanes_split_the_time_none_works() {
        let p = profile(vec![
            track(1, LaneClass::Main, vec![wait(0, 100)]),
            track(2, LaneClass::Analysis, vec![wait(0, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [50, 50]);
    }

    #[test]
    fn a_collector_pause_takes_its_time_from_every_other_lane() {
        let p = profile(vec![
            track(1, LaneClass::Gc, vec![pause(40, 20)]),
            track(2, LaneClass::Execution, vec![work(0, 100)]),
            track(3, LaneClass::Execution, vec![work(0, 100)]),
            track(4, LaneClass::Main, vec![wait(0, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [20, 40, 40, 0]);
        assert_eq!(shares(&p, 40, 60), [20, 0, 0, 0]);
    }

    #[test]
    fn the_critical_path_lane_takes_no_share_and_weighs_its_own_duration() {
        let p = profile(vec![
            track(
                0,
                LaneClass::CriticalPath,
                vec![slice("action 'a'", "critical path component", 0, 100)],
            ),
            track(1, LaneClass::Execution, vec![work(0, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [100, 100]);
        assert_eq!(shares(&p, 20, 30), [10, 10]);
        // The replay lane weighs elapsed time whether or not it has a span
        // there; the sharing lane is given none where none is open.
        assert_eq!(shares(&p, 100, 150), [50, 0]);
    }

    #[test]
    fn spans_overlapping_without_nesting_keep_their_lane_open_once() {
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![work(0, 10), work(5, 7)],
        )]);
        assert_eq!(shares(&p, 0, 20), [12]);
    }

    #[test]
    fn the_innermost_of_overlapping_spans_is_the_latest_begun_still_open() {
        // The wait begins inside the work and outlives it: the lane is parked
        // from 6 on, working only before it.
        let p = profile(vec![
            track(1, LaneClass::Main, vec![work(0, 10), wait(6, 14)]),
            track(2, LaneClass::Execution, vec![work(0, 20)]),
        ]);
        assert_eq!(shares(&p, 0, 6), [3, 3]);
        assert_eq!(shares(&p, 6, 20), [0, 14]);
    }

    #[test]
    fn a_slice_of_zero_duration_opens_no_span() {
        let p = profile(vec![
            track(1, LaneClass::Execution, vec![work(0, 100)]),
            track(2, LaneClass::Execution, vec![work(50, 0)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [100, 0]);
    }

    #[test]
    fn a_gap_between_spans_goes_to_no_lane() {
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![work(0, 10), work(30, 10)],
        )]);
        assert_eq!(shares(&p, 0, 40), [20]);
        assert_eq!(shares(&p, 10, 30), [0]);
    }

    #[test]
    fn abutting_stretches_weigh_what_the_stretch_they_make_up_does() {
        // Three lanes share every microsecond, so no single one divides evenly.
        let p = profile(vec![
            track(1, LaneClass::Execution, vec![work(0, 100)]),
            track(2, LaneClass::Execution, vec![work(0, 100)]),
            track(3, LaneClass::Execution, vec![work(0, 100)]),
        ]);
        let share = WallShare::new(&p, 2);
        let mut lane = share.lane(0);
        assert_eq!(lane.between(0, 100), 33);
        for cut in 0..=100 {
            assert_eq!(lane.between(0, cut) + lane.between(cut, 100), 33, "{cut}");
        }
        // Microsecond by microsecond the thirds still add up to the whole.
        let stepped: i64 = (0..100).map(|t| share.lane(0).between(t, t + 1)).sum();
        assert_eq!(stepped, 33);
        // A stretch never outweighs one holding it.
        for start in 0..100 {
            for end in start..=100 {
                let inner = lane.between(start, end);
                assert!((0..=33).contains(&inner), "[{start}, {end})");
            }
        }
    }

    #[test]
    fn a_stretch_weighs_the_same_whatever_was_weighed_before_it() {
        let p = profile(vec![
            track(
                1,
                LaneClass::Main,
                vec![wait(0, 1000), work(100, 50), work(400, 200)],
            ),
            track(2, LaneClass::Gc, vec![pause(120, 10), pause(700, 30)]),
            track(
                3,
                LaneClass::Execution,
                vec![work(90, 300), work(450, 10), work(900, 300)],
            ),
        ]);
        let share = WallShare::new(&p, 2);
        for lane in 0..3 {
            let mut reused = share.lane(lane);
            // Descending starts leave every hint ahead of the next query.
            for start in (0..1300).rev().step_by(7) {
                for len in [1, 13, 250, 1300] {
                    assert_eq!(
                        reused.between(start, start + len),
                        share.lane(lane).between(start, start + len),
                        "lane {lane} [{start}, {})",
                        start + len
                    );
                }
            }
        }
    }

    #[test]
    fn the_lanes_shares_sum_to_the_time_any_of_them_is_open() {
        // Open from 0 to 260 except for the gap from 120 to 140.
        let p = profile(vec![
            track(1, LaneClass::Main, vec![wait(0, 100)]),
            track(2, LaneClass::Execution, vec![work(20, 100), work(140, 60)]),
            track(3, LaneClass::Execution, vec![work(60, 40), work(160, 100)]),
            track(4, LaneClass::Gc, vec![pause(70, 10)]),
        ]);
        assert_eq!(shares(&p, 0, 300).iter().sum::<i64>(), 240);
    }

    #[test]
    fn a_fetch_host_is_given_nothing_while_its_worker_works() {
        let p = profile(vec![
            track(1, LaneClass::Analysis, vec![fetch_host("r", 0, 100)]),
            track(2, LaneClass::RepoFetch, vec![fetch_worker("r", 0, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [0, 100]);
    }

    #[test]
    fn a_fetch_host_works_wherever_its_worker_has_no_span_open() {
        let p = profile(vec![
            track(1, LaneClass::Analysis, vec![fetch_host("r", 0, 100)]),
            track(2, LaneClass::RepoFetch, vec![fetch_worker("r", 40, 30)]),
            track(3, LaneClass::Execution, vec![work(0, 100)]),
        ]);
        // The host halves with the third lane up to 40 and from 70 on, the
        // worker in between.
        assert_eq!(shares(&p, 0, 100), [35, 15, 50]);
        assert_eq!(shares(&p, 0, 40), [20, 0, 20]);
        assert_eq!(shares(&p, 40, 70), [0, 15, 15]);
        // A stretch reaching across the worker's ends weighs each side's part.
        assert_eq!(shares(&p, 30, 50), [5, 5, 10]);
        assert_eq!(shares(&p, 60, 90), [10, 5, 15]);
    }

    #[test]
    fn a_fetch_host_with_no_worker_works_throughout() {
        let p = profile(vec![
            track(1, LaneClass::Analysis, vec![fetch_host("r", 0, 100)]),
            track(2, LaneClass::Execution, vec![work(0, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [50, 50]);
    }

    #[test]
    fn workers_running_together_split_the_time_their_hosts_wait() {
        let p = profile(vec![
            track(1, LaneClass::Analysis, vec![fetch_host("r", 0, 100)]),
            track(2, LaneClass::Analysis, vec![fetch_host("s", 0, 100)]),
            track(3, LaneClass::RepoFetch, vec![fetch_worker("r", 0, 100)]),
            track(4, LaneClass::RepoFetch, vec![fetch_worker("s", 0, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [0, 0, 50, 50]);
    }

    #[test]
    fn another_repositorys_worker_leaves_a_fetch_host_working() {
        let p = profile(vec![
            track(1, LaneClass::Analysis, vec![fetch_host("r", 0, 100)]),
            track(2, LaneClass::RepoFetch, vec![fetch_worker("s", 0, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [50, 50]);
    }

    #[test]
    fn a_repository_named_by_the_head_of_anothers_name_is_not_that_one() {
        let hosts = || {
            vec![
                track(1, LaneClass::Analysis, vec![fetch_host("foo", 0, 100)]),
                track(2, LaneClass::Analysis, vec![fetch_host("foo-1.0", 0, 100)]),
            ]
        };
        let with_worker_of = |repository: &str| {
            let mut tracks = hosts();
            tracks.push(track(
                3,
                LaneClass::RepoFetch,
                vec![fetch_worker(repository, 0, 100)],
            ));
            profile(tracks)
        };
        assert_eq!(shares(&with_worker_of("foo"), 0, 100), [0, 50, 50]);
        assert_eq!(shares(&with_worker_of("foo-1.0"), 0, 100), [50, 0, 50]);
        assert_eq!(shares(&with_worker_of("fo"), 0, 100), [33, 33, 33]);
    }

    #[test]
    fn every_host_span_of_a_restarted_fetch_waits_on_its_one_worker() {
        let p = profile(vec![
            track(
                1,
                LaneClass::Analysis,
                vec![
                    fetch_host("r", 0, 30),
                    fetch_host("r", 40, 20),
                    fetch_host("r", 70, 30),
                ],
            ),
            track(2, LaneClass::RepoFetch, vec![fetch_worker("r", 10, 80)]),
        ]);
        // The host is alone up to 10 and from 90 on.
        assert_eq!(shares(&p, 0, 100), [20, 80]);
        assert_eq!(shares(&p, 10, 90), [0, 80]);
    }

    #[test]
    fn a_fetch_host_waiting_while_no_lane_works_shares_with_the_parked() {
        // The worker's lane is itself parked from 20 to 50.
        let p = profile(vec![
            track(1, LaneClass::Analysis, vec![fetch_host("r", 0, 100)]),
            track(
                2,
                LaneClass::RepoFetch,
                vec![fetch_worker("r", 0, 100), wait(20, 30)],
            ),
        ]);
        assert_eq!(shares(&p, 0, 100), [15, 85]);
        assert_eq!(shares(&p, 20, 50), [15, 15]);
    }

    #[test]
    fn a_collector_pause_takes_its_time_from_a_fetch_and_its_host() {
        let p = profile(vec![
            track(1, LaneClass::Gc, vec![pause(40, 20)]),
            track(2, LaneClass::Analysis, vec![fetch_host("r", 0, 100)]),
            track(3, LaneClass::RepoFetch, vec![fetch_worker("r", 0, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [20, 0, 80]);
    }

    #[test]
    fn work_nested_in_a_fetch_hosts_span_shares_with_the_worker() {
        let p = profile(vec![
            track(
                1,
                LaneClass::Analysis,
                vec![fetch_host("r", 0, 100), work(20, 30)],
            ),
            track(2, LaneClass::RepoFetch, vec![fetch_worker("r", 0, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [15, 85]);
    }

    #[test]
    fn a_worker_span_on_the_hosts_own_lane_does_not_park_it() {
        // The host span is the innermost on its lane from 10 to 60, inside a
        // worker span of its repository.
        let p = profile(vec![
            track(
                1,
                LaneClass::Analysis,
                vec![fetch_worker("r", 0, 100), fetch_host("r", 10, 50)],
            ),
            track(2, LaneClass::Execution, vec![work(0, 100)]),
        ]);
        assert_eq!(shares(&p, 0, 100), [50, 50]);
    }

    #[test]
    fn a_command_a_repository_rule_runs_parks_no_host() {
        let command = slice("local: r", "Starlark repository function call", 0, 100);
        let p = profile(vec![
            track(1, LaneClass::Analysis, vec![fetch_host("r", 0, 100)]),
            track(2, LaneClass::RepoFetch, vec![command]),
        ]);
        assert_eq!(shares(&p, 0, 100), [50, 50]);
    }

    #[test]
    fn workers_of_one_repository_overlapping_park_its_host_once() {
        let p = profile(vec![
            track(1, LaneClass::Analysis, vec![fetch_host("r", 0, 100)]),
            track(2, LaneClass::RepoFetch, vec![fetch_worker("r", 10, 50)]),
            track(3, LaneClass::RepoFetch, vec![fetch_worker("r", 30, 20)]),
            track(4, LaneClass::RepoFetch, vec![fetch_worker("r", 40, 40)]),
        ]);
        // Parked from 10 to 80: one worker to 30, three to 50, two to 60, one
        // to 80.
        assert_eq!(shares(&p, 0, 100), [30, 33, 8, 28]);
        assert_eq!(shares(&p, 10, 80), [0, 33, 8, 28]);
    }

    #[test]
    fn a_fetch_hosts_stretches_weigh_what_the_span_they_make_up_does() {
        let p = profile(vec![
            track(
                1,
                LaneClass::Analysis,
                vec![work(0, 20), fetch_host("r", 20, 60), work(80, 20)],
            ),
            track(2, LaneClass::RepoFetch, vec![fetch_worker("r", 40, 15)]),
            track(3, LaneClass::Execution, vec![work(0, 100)]),
            track(4, LaneClass::Execution, vec![work(0, 100)]),
        ]);
        let share = WallShare::new(&p, 2);
        for l in 0..4 {
            let mut lane = share.lane(l);
            let whole = lane.between(0, 100);
            for cut in 0..=100 {
                assert_eq!(
                    lane.between(0, cut) + lane.between(cut, 100),
                    whole,
                    "lane {l} cut at {cut}"
                );
            }
        }
        // Thirds everywhere: the host or its worker beside the two others.
        assert_eq!(shares(&p, 0, 100), [28, 5, 33, 33]);
    }

    #[test]
    fn a_share_longer_than_a_reading_holds_reads_as_the_longest_it_does() {
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![work(0, 2 * MAX_READING_US)],
        )]);
        assert_eq!(shares(&p, 0, 2 * MAX_READING_US), [MAX_READING_US]);
        assert_eq!(shares(&p, 0, 1000), [1000]);
    }

    /// The repository a slice of a lane that shares fetches as `side`.
    fn fetched<'p>(
        p: &'p Profile,
        thread: &bviz_trace_profile::ThreadTrack,
        slice: usize,
        side: FetchSide,
    ) -> Option<Repository<'p>> {
        if matches!(thread.lane_class, LaneClass::Gc | LaneClass::CriticalPath) {
            return None;
        }
        let category = p.str(p.details(thread)[slice].category);
        if fetch_side(category) != Some(side) {
            return None;
        }
        side.repository(p.str(p.slices(thread)[slice].name))
    }

    /// Each sharing lane's exact share of `[0, t)` for every `t` up to
    /// `until_us`, found by giving out one microsecond at a time; `None` for
    /// the critical-path lane.
    fn shares_by_microsecond(p: &Profile, until_us: i64) -> Vec<Option<Vec<f64>>> {
        let threads = p.threads();
        let open_at = |s: &Slice, t: i64| s.dur_us > 0 && s.start_us <= t && t < s.end_us();
        // Whether a lane other than `host_lane` has a worker of `repository`
        // open at `t`, at any depth.
        let worked_elsewhere = |host_lane: usize, repository: Repository, t: i64| {
            threads.iter().enumerate().any(|(lane, thread)| {
                lane != host_lane
                    && p.slices(thread).iter().enumerate().any(|(i, s)| {
                        open_at(s, t)
                            && fetched(p, thread, i, FetchSide::Worker) == Some(repository)
                    })
            })
        };
        let mut given: Vec<Option<Vec<f64>>> = threads
            .iter()
            .map(|thread| (thread.lane_class != LaneClass::CriticalPath).then(|| Vec::from([0.0])))
            .collect();
        for t in 0..until_us {
            let tiers: Vec<Option<Tier>> = threads
                .iter()
                .enumerate()
                .map(|(lane, thread)| {
                    if thread.lane_class == LaneClass::CriticalPath {
                        return None;
                    }
                    let details = p.details(thread);
                    let innermost = p.slices(thread).iter().rposition(|s| open_at(s, t))?;
                    let hosted = fetched(p, thread, innermost, FetchSide::Host);
                    Some(if thread.lane_class == LaneClass::Gc {
                        Tier::Pause
                    } else if parks_thread(p.str(details[innermost].category))
                        || hosted.is_some_and(|repository| worked_elsewhere(lane, repository, t))
                    {
                        Tier::Parked
                    } else {
                        Tier::Working
                    })
                })
                .collect();
            let lanes_in = |tier: Tier| tiers.iter().filter(|&&x| x == Some(tier)).count();
            let foremost = [Tier::Pause, Tier::Working, Tier::Parked]
                .into_iter()
                .find(|&tier| lanes_in(tier) > 0);
            for (given, tier) in given.iter_mut().zip(&tiers) {
                let Some(given) = given else { continue };
                let share = match foremost {
                    Some(foremost) if *tier == Some(foremost) => 1.0 / lanes_in(foremost) as f64,
                    _ => 0.0,
                };
                given.push(given[t as usize] + share);
            }
        }
        given
    }

    #[test]
    fn random_profiles_split_as_they_do_one_microsecond_at_a_time() {
        /// How many random profiles are checked.
        const PROFILES: usize = 150;
        /// Every span begins before this.
        const HORIZON_US: i64 = 80;
        /// No span reaches past this.
        const UNTIL_US: i64 = 120;
        /// How far a fixed-point reading may fall from the exact share, in
        /// microseconds: far above what truncation costs over so few changes,
        /// far below a sharer's part of one microsecond.
        const READING_SLACK_US: f64 = 1e-3;
        /// Queries made of each lane through one reused handle.
        const QUERIES: usize = 40;
        const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

        let mut rng = Rng::with_seed(SEED);
        for case in 0..PROFILES {
            let p = random_profile(&mut rng, HORIZON_US);
            let exact = shares_by_microsecond(&p, UNTIL_US);
            let share = WallShare::new(&p, 1 + case % 3);
            for (l, exact) in exact.iter().enumerate() {
                let (Lane::Sharing(runs), Some(exact)) = (&share.lanes[l], exact) else {
                    continue;
                };
                let mut hints = Hints::default();
                for t in 0..=UNTIL_US {
                    let reading = runs.reading_at(&share.clocks, t, &mut hints) as f64
                        / (1u64 << FRACTION_BITS) as f64;
                    let want = exact[t as usize];
                    assert!(
                        (reading - want).abs() < READING_SLACK_US,
                        "case {case} lane {l} at {t}: {reading} for {want}"
                    );
                }
                // Stretches in no order, some reaching outside the profile.
                let mut reused = share.lane(l);
                for _ in 0..QUERIES {
                    let start = rng.i64(-10..UNTIL_US + 10);
                    let end = start + rng.i64(0..60);
                    let got = share.lane(l).between(start, end);
                    assert_eq!(reused.between(start, end), got, "case {case} lane {l}");
                    let at = |t: i64| exact[t.clamp(0, UNTIL_US) as usize];
                    // Each end rounds to the nearest whole microsecond.
                    assert!(
                        (got as f64 - (at(end) - at(start))).abs() <= 1.0 + READING_SLACK_US,
                        "case {case} lane {l} [{start}, {end}): {got}"
                    );
                }
            }
        }
    }
}
