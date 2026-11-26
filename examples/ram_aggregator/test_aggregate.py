"""Locked equivalence and unit tests for examples/ram_aggregator."""

from aggregate import SERVICES, summarize_events


def test_small_batch() -> None:
    res = summarize_events(400)
    assert sorted(res.keys()) == sorted(SERVICES)
    assert sum(v["count"] for v in res.values()) == 400
    for svc, stats in res.items():
        assert stats["count"] == 50, f"unexpected count for {svc}: {stats}"
        assert stats["min_us"] <= stats["max_us"]
        assert stats["sum_us"] >= stats["min_us"] * stats["count"]
        assert stats["total_bytes"] > 0
        assert stats["errors"] > 0


def test_deterministic_fields() -> None:
    a = summarize_events(80)
    b = summarize_events(80)
    assert a == b
    assert a["auth"]["count"] == 10
    assert a["checkout"]["errors"] == 2


if __name__ == "__main__":
    test_small_batch()
    test_deterministic_fields()
    print("ok: 2 tests passed")
