const { test } = require("node:test");
const assert = require("node:assert/strict");
const { load } = require("./provider-sync-harness.cjs");

test("date helpers reject invalid types and out-of-range dates", () => {
  const { util } = load("notion").ctx;
  for (const value of [null, undefined, true, false, [], {}, "", " ", NaN, Infinity, 1e100]) {
    assert.equal(util.toIso(value), null, String(value));
  }
  assert.equal(util.toIso(1788609600), "2026-09-05T12:00:00.000Z");
  assert.equal(util.toIso(1788609600000), "2026-09-05T12:00:00.000Z");
});

test("monthly pace respects leap years, year boundaries, and clamped billing dates", () => {
  const { util } = load("notion").ctx;
  for (const [reset, days] of [["2026-03-01", 28], ["2024-03-01", 29], ["2026-08-01", 31], ["2027-01-01", 31], ["2026-03-31", 31]]) {
    assert.equal(util.calendarMonthDuration(reset), days * 86400000, reset);
  }
  assert.equal(util.calendarMonthDuration(null), null);
});
