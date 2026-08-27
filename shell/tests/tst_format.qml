import QtQuick
import QtTest
import "../Format.js" as Fmt

// The units. Every one of these takes what the kernel handed over — KiB, kHz,
// microwatts, millidegrees — and none of the call sites convert first, so a
// wrong divisor here is a wrong number everywhere and looks plausible.
TestCase {
  name: "Format"

  function test_bytes_take_kib_and_step_at_a_thousand() {
    compare(Fmt.bytes(0), "0B")
    // One KiB is 1024 bytes, which is already past the thousand step.
    compare(Fmt.bytes(1), "1.0KB")
    compare(Fmt.bytes(0.5), "512B")
    compare(Fmt.bytes(1024), "1.0MB")
    compare(Fmt.bytes(6100000), "6.2GB")
  }

  function test_rates_are_bytes_per_second() {
    compare(Fmt.rate(1048576), "1.0MB/s")
  }

  function test_percentages_gain_a_decimal_when_small() {
    compare(Fmt.pct(0), "0.0%")
    compare(Fmt.pct(9.94), "9.9%")
    compare(Fmt.pct(20), "20%")
  }

  function test_clock_speeds_come_in_as_khz() {
    compare(Fmt.ghz(827000), "827MHz")
    compare(Fmt.ghz(3833333), "3.8GHz")
  }

  function test_watts_come_in_as_microwatts_and_lose_their_sign() {
    // The direction is carried by an arrow beside the figure, not by a minus.
    compare(Fmt.watts(-6732000), "6.7W")
    compare(Fmt.watts(43300000), "43.3W")
  }

  function test_temperature_comes_in_as_millidegrees() {
    compare(Fmt.degC(46000), "46°C")
  }

  function test_fan_speeds_shorten_at_four_figures() {
    compare(Fmt.krpm(900), "900")
    compare(Fmt.krpm(3892), "3.9k")
  }

  function test_durations_read_as_two_units() {
    compare(Fmt.dur(0), "0s")
    compare(Fmt.dur(45), "45s")
    compare(Fmt.dur(90), "1m30s")
    compare(Fmt.dur(3600), "1h00m")
    compare(Fmt.dur(90000), "1d01h")
    compare(Fmt.dur(-5), "0s", "a negative span is nothing, not a negative reading")
  }

  // A seam is a dozen pixels wide; the exact figure is a hover away.
  function test_brief_durations_are_one_unit() {
    compare(Fmt.briefDur(8 * 3600), "8h")
    compare(Fmt.briefDur(2 * 86400), "2d")
    compare(Fmt.briefDur(20), "1m", "anything shorter than a minute still reads as one")
  }

  function test_each_metric_gets_its_own_unit() {
    compare(Fmt.forMetric("mem")(1024), "1.0MB")
    compare(Fmt.forMetric("cpu")(20), "20%")
    compare(Fmt.forMetric("io")(1048576), "1.0MB/s")
    compare(Fmt.forMetric("nonsense")(1024), "1.0MB", "an unknown metric falls back to bytes")
  }
}
