import unittest

from parser import analyze_logs, generate_logs, parse_line


class TestLogParser(unittest.TestCase):
    def test_parse_valid_and_invalid_lines(self) -> None:
        valid = (
            "2026-11-26T09:00:01Z ip=10.1.2.3 method=GET path=/api/v1/users "
            "status=200 latency_ms=42 id=req-0001"
        )
        rec = parse_line(valid)
        self.assertIsNotNone(rec)
        assert rec is not None
        self.assertEqual(rec["ip"], "10.1.2.3")
        self.assertEqual(rec["status"], 200)
        self.assertEqual(rec["latency"], 42)
        self.assertIsNone(parse_line("corrupt line without fields"))

    def test_analyze_small_stream(self) -> None:
        lines = [
            "2026-11-26T09:00:00Z ip=10.0.0.1 method=GET path=/api/v1/a status=200 latency_ms=10 id=req-1",
            "2026-11-26T09:00:01Z ip=10.0.0.1 method=GET path=/api/v1/a status=500 latency_ms=50 id=req-2",
            "2026-11-26T09:00:02Z ip=10.0.0.2 method=GET path=/api/v1/b status=503 latency_ms=20 id=req-3",
            "garbage",
        ]
        summary = analyze_logs(lines)
        self.assertEqual(summary["parsed"], 3)
        self.assertEqual(summary["unique_ips"], 2)
        self.assertEqual(summary["total_latency_ms"], 80)
        self.assertEqual(summary["status_buckets"], {"2xx": 1, "5xx": 2})
        self.assertEqual(summary["top_5xx_paths"], [["/api/v1/a", 1], ["/api/v1/b", 1]] if isinstance(summary["top_5xx_paths"][0], list) else [("/api/v1/a", 1), ("/api/v1/b", 1)])

    def test_deterministic_generation(self) -> None:
        a = analyze_logs(generate_logs(500))
        b = analyze_logs(generate_logs(500))
        self.assertEqual(a, b)
        self.assertEqual(a["parsed"], 500)


if __name__ == "__main__":
    unittest.main()
