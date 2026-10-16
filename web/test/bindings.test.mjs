// Exercises the wasm bindings end to end over the synthetic fixture:
// node --test web/test  (after `wasm-pack build wasm --target web --out-dir ../web/pkg`)
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import zlib from "node:zlib";
import { initSync, Trace } from "../pkg/bviz_wasm.js";
import { syntheticProfile } from "../fixture.mjs";

initSync({ module: fs.readFileSync(new URL("../pkg/bviz_wasm_bg.wasm", import.meta.url)) });
const json = new TextEncoder().encode(JSON.stringify(syntheticProfile()));

test("plain and gzipped profiles parse to the same lanes", () => {
  const plain = JSON.parse(new Trace(json, 18, 16).summary());
  const gz = JSON.parse(new Trace(zlib.gzipSync(json), 18, 16).summary());
  assert.deepEqual(gz.lanes, plain.lanes);
  assert.equal(plain.bazel_version, "release 9.0.0");
  assert.ok(plain.end_us - plain.start_us >= 5_990_000, plain.end_us - plain.start_us);
  assert.equal(plain.markers.length, 3);
  assert.ok(plain.lanes.some((l) => l.class === "CriticalPath"));
  assert.equal(plain.lanes.filter((l) => l.class === "Execution").length, 4);
});

test("culling returns packed boxes inside the viewport", () => {
  const t = new Trace(json, 18, 16);
  const s = JSON.parse(t.summary());
  const v = t.visible(s.start_us, 1000 / 6_000_000, 1000, 400, 0, 1);
  assert.equal(v.length % 7, 0);
  assert.ok(v.length / 7 > 10);
  for (let i = 0; i < v.length; i += 7) {
    assert.ok(v[i + 1] >= 0 && v[i + 1] < 400);
    assert.ok(v[i + 4] >= 0 && v[i + 4] < 16);
    assert.ok(t.slice_name(v[i + 5], v[i + 6]).length > 0);
  }
  const info = JSON.parse(t.slice_info(v[5], v[6]));
  assert.ok(info.dur_us > 0);
});

test("wall-share flame totals the busy wall time; summed duration exceeds it", () => {
  const t = new Trace(json, 18, 16);
  const wall = t.build_flame("top-down", "kind", "wall", 0, 0);
  assert.ok(wall <= 6_000_000 && wall > 5_000_000, wall);
  const summed = t.build_flame("top-down", "kind", "duration", 0, 0);
  assert.ok(summed > wall);
  const windowed = t.build_flame("top-down", "kind", "duration", 1_000_000, 2_000_000);
  assert.ok(windowed < summed);
  t.build_flame("bottom-up", "name", "wall", 0, 0);
  const boxes = t.flame_boxes(0, 800, 800, 18, 2);
  assert.ok(boxes.length / 7 > 3);
  assert.equal(JSON.parse(t.flame_box_info(0)).name, "all");
  assert.ok(t.flame_height(18) > 0);
  const stats = JSON.parse(t.stats(5));
  assert.ok(stats.length > 0 && stats[0].self_us >= stats.at(-1).self_us);
});

test("critical path parts add up to the wall time", () => {
  const cp = JSON.parse(new Trace(json, 18, 16).summary()).critical_path;
  assert.equal(cp.steps.length, 6);
  const sum = Object.values(cp.parts).reduce((a, b) => a + b, 0);
  assert.ok(Math.abs(sum - cp.wall_us) < 1, `${sum} vs ${cp.wall_us}`);
  assert.ok(cp.parts.Action > 0);
});

test("a file that is not a profile is an error or empty, never a panic", () => {
  // bviz reads JSON without traceEvents as an empty profile.
  const empty = JSON.parse(new Trace(new TextEncoder().encode('{"hello":1}'), 18, 16).summary());
  assert.equal(empty.lanes.length, 0);
  assert.throws(() => new Trace(new TextEncoder().encode("not json"), 18, 16), /./);
  assert.throws(() => new Trace(new Uint8Array([0x1f, 0x8b, 8, 0, 1, 2, 3]), 18, 16), /./);
});
