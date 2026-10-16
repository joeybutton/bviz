// A deterministic, entirely synthetic Bazel JSON trace profile: a main thread,
// a garbage collector, four execution lanes running nested actions, a
// critical path naming the first lane's actions, phase markers and counters.
// It stands in for a real profile in the example page and the tests, and
// holds no paths or names from any real build.

export function syntheticProfile() {
  const thread = (tid, name) => ({ name: "thread_name", ph: "M", pid: 1, tid, args: { name } });
  const events = [
    thread(0, "Critical Path"),
    thread(1, "Main Thread"),
    thread(9, "Garbage Collector"),
    { cat: "build phase marker", name: "Launch Blaze", ph: "i", ts: 0, pid: 1, tid: 1 },
    { cat: "build phase marker", name: "Initialize command", ph: "i", ts: 20_000, pid: 1, tid: 1 },
    { cat: "build phase marker", name: "Execution", ph: "i", ts: 1_000_000, pid: 1, tid: 1 },
    { cat: "general information", name: "buildTargets", ph: "X", ts: 10, dur: 5_990_000, pid: 1, tid: 1 },
    { cat: "general information", name: "runAnalysisPhase", ph: "X", ts: 50_000, dur: 900_000, pid: 1, tid: 1 },
    { cat: "gc notification", name: "minor GC", ph: "X", ts: 2_000_000, dur: 100_000, pid: 1, tid: 9 },
  ];
  for (let lane = 0; lane < 4; lane++) {
    const tid = 10 + lane;
    events.push(thread(tid, `skyframe-evaluator-execution-${lane}`));
    let t = 1_000_000 + lane * 50_000;
    for (let i = 0; i < 6; i++) {
      const label = `//demo/lib${lane}:part${i}`;
      const mnemonic = i % 2 ? "Rustc" : "Javac";
      const d = 300_000 + 100_000 * ((lane + i) % 3);
      events.push(
        { cat: "action processing", name: `Compiling ${label}`, ph: "X", ts: t, dur: d, pid: 1, tid,
          args: { target: label, mnemonic }, out: `bazel-out/fastbuild/bin/demo/lib${lane}/part${i}.out` },
        { cat: "action dependency checking", name: "action dependency checking", ph: "X", ts: t + 2_000, dur: 6_000, pid: 1, tid },
        { cat: "general information", name: "sandbox.createFileSystem", ph: "X", ts: t + 9_000, dur: 4_000, pid: 1, tid },
        { cat: "general information", name: "subprocess.run", ph: "X", ts: t + 15_000, dur: d - 30_000, pid: 1, tid },
      );
      t += d + 20_000;
    }
  }
  // The critical path names the first lane's actions as Bazel logs them.
  let t = 1_000_000;
  for (let i = 0; i < 6; i++) {
    const d = 300_000 + 100_000 * (i % 3);
    events.push({ cat: "critical path component", name: `action 'Compiling //demo/lib0:part${i}'`, ph: "X", ts: t, dur: d, pid: 1, tid: 0 });
    t += d + 20_000;
  }
  for (let i = 0; i < 30; i++) {
    events.push(
      { name: "action count", ph: "C", ts: 200_000 * i, pid: 1, tid: 1, args: { action: i >= 5 && i <= 25 ? 4 : 0 } },
      { name: "CPU usage (Bazel)", ph: "C", ts: 200_000 * i, pid: 1, tid: 1, args: { cpu: 1.5 + (i % 4) } },
    );
  }
  return {
    otherData: { bazel_version: "release 9.0.0", build_id: "00000000-0000-4000-8000-000000000000" },
    traceEvents: events,
  };
}
