use crate::session::{CompareResult, MetricTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgTheme {
    Dark,
    Light,
}

impl SvgTheme {
    pub fn parse_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "dark" => Ok(Self::Dark),
            "light" => Ok(Self::Light),
            other => Err(format!(
                "invalid SVG theme '{other}': expected 'dark' or 'light'"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SvgStep {
    pub label: String,
    pub subtitle: String,
    pub value: f64,
    pub gain: f64,
    pub p_value: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct SvgChartData {
    pub repo_name: String,
    pub metric: MetricTarget,
    pub baseline_value: f64,
    pub best_value: f64,
    pub cumulative_gain: f64,
    pub rss_before_kb: Option<u64>,
    pub rss_after_kb: Option<u64>,
    pub sha256_prefix: String,
    pub locked_files: usize,
    pub kept_count: usize,
    pub reverted_count: usize,
    pub steps: Vec<SvgStep>,
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn format_metric_value(val: f64, metric: MetricTarget) -> String {
    match metric {
        MetricTarget::Wall | MetricTarget::Pareto => format!("{val:.2} ms"),
        MetricTarget::Rss => {
            if val >= 1024.0 {
                format!("{:.1} MiB", val / 1024.0)
            } else {
                format!("{val:.0} KiB")
            }
        }
    }
}

/// Renders a self-contained dark or light SVG chart of an `oa` optimization session.
pub fn render_session_svg(data: &SvgChartData, theme: SvgTheme) -> String {
    let (
        bg,
        border,
        divider,
        text_primary,
        text_secondary,
        text_muted,
        track_bg,
        bar_base,
        bar_accent,
        pill_bg,
        pill_text,
    ) = match theme {
        SvgTheme::Dark => (
            "#0d1117",
            "#30363d",
            "#21262d",
            "#e6edf3",
            "#8b949e",
            "#6e7681",
            "#161b22",
            "#30363d",
            if data.metric == MetricTarget::Rss {
                "#58a6ff"
            } else {
                "#3fb950"
            },
            "#161b22",
            if data.metric == MetricTarget::Rss {
                "#58a6ff"
            } else {
                "#3fb950"
            },
        ),
        SvgTheme::Light => (
            "#ffffff",
            "#d0d7de",
            "#eaeef2",
            "#1f2328",
            "#57606a",
            "#6e7781",
            "#f6f8fa",
            "#d8dee4",
            if data.metric == MetricTarget::Rss {
                "#0969da"
            } else {
                "#1a7f37"
            },
            "#f6f8fa",
            if data.metric == MetricTarget::Rss {
                "#0969da"
            } else {
                "#1a7f37"
            },
        ),
    };

    let width = 820usize;
    let bar_x = 246usize;
    let bar_max_w = 340.0f64;
    let row_h = 46usize;

    // Build rows: always start with Baseline, then up to 6 kept steps (or Best if none).
    let mut rows: Vec<(String, String, f64, f64, bool, Option<f64>)> = Vec::new();
    rows.push((
        "Baseline".to_string(),
        "refs/oa/baseline".to_string(),
        data.baseline_value,
        1.0,
        true,
        None,
    ));

    if data.steps.is_empty() {
        rows.push((
            "Best".to_string(),
            "refs/oa/best".to_string(),
            data.best_value,
            data.cumulative_gain,
            false,
            None,
        ));
    } else {
        let start = data.steps.len().saturating_sub(6);
        for s in &data.steps[start..] {
            rows.push((
                truncate_chars(&s.label, 28),
                truncate_chars(&s.subtitle, 32),
                s.value,
                s.gain,
                false,
                s.p_value,
            ));
        }
    }

    let max_val = rows.iter().map(|r| r.2).fold(0.0f64, f64::max).max(1e-6);

    let header_h = 74usize;
    let footer_h = 50usize;
    let height = header_h + rows.len() * row_h + footer_h;

    let metric_label = match data.metric {
        MetricTarget::Wall => "wall latency",
        MetricTarget::Rss => "peak RSS",
        MetricTarget::Pareto => "pareto (wall + RSS)",
    };

    let headline = format!("{} ({metric_label})", truncate_chars(&data.repo_name, 36));
    let badge_text = format!(
        "{:.2}x {} | SHA-256 {}",
        data.cumulative_gain,
        if data.metric == MetricTarget::Rss {
            "less RAM"
        } else {
            "faster"
        },
        truncate_chars(&data.sha256_prefix, 12)
    );

    let rss_note = match (data.rss_before_kb, data.rss_after_kb) {
        (Some(b), Some(a)) if b > 0 => {
            let pct = ((a as f64 - b as f64) / b as f64) * 100.0;
            format!(
                " | Peak RSS {:.1} -> {:.1} MiB ({:+.1}%)",
                b as f64 / 1024.0,
                a as f64 / 1024.0,
                pct
            )
        }
        _ => String::new(),
    };

    let footer_left = format!(
        "{} locked files | {} kept, {} reverted{rss_note}",
        data.locked_files, data.kept_count, data.reverted_count
    );

    let mut svg = String::with_capacity(4096);
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\" role=\"img\" aria-label=\"{}\">\n",
        xml_escape(&format!("oa optimization report for {}", data.repo_name))
    ));
    svg.push_str("  <style>\n");
    svg.push_str(&format!(
        "    .title {{ font: 600 15px -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif; fill: {text_primary}; }}\n"
    ));
    svg.push_str(&format!(
        "    .sub {{ font: 400 11.5px -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif; fill: {text_secondary}; }}\n"
    ));
    svg.push_str(&format!(
        "    .label {{ font: 600 12.5px -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif; fill: {text_primary}; }}\n"
    ));
    svg.push_str(&format!(
        "    .meta {{ font: 400 10.5px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; fill: {text_muted}; }}\n"
    ));
    svg.push_str(&format!(
        "    .val {{ font: 600 12.5px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; fill: {text_primary}; }}\n"
    ));
    svg.push_str(&format!(
        "    .gain {{ font: 700 13px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; fill: {bar_accent}; }}\n"
    ));
    svg.push_str(&format!(
        "    .pill {{ font: 600 11px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; fill: {pill_text}; }}\n"
    ));
    svg.push_str("  </style>\n");

    // Outer card
    svg.push_str(&format!(
        "  <rect x=\"1\" y=\"1\" width=\"{}\" height=\"{}\" rx=\"10\" fill=\"{bg}\" stroke=\"{border}\" stroke-width=\"1.2\"/>\n",
        width - 2,
        height - 2
    ));

    // Header
    svg.push_str(&format!(
        "  <text x=\"24\" y=\"32\" class=\"title\">{}</text>\n",
        xml_escape(&headline)
    ));
    svg.push_str(
        "  <text x=\"24\" y=\"50\" class=\"sub\">Balanced interleaved A/B pairs | shorter bar is better</text>\n",
    );

    // Verified pill on top right
    let pill_w = (badge_text.chars().count() * 7 + 22).clamp(180, 310);
    let pill_x = width - 24 - pill_w;
    svg.push_str(&format!(
        "  <rect x=\"{pill_x}\" y=\"18\" width=\"{pill_w}\" height=\"26\" rx=\"13\" fill=\"{pill_bg}\" stroke=\"{border}\" stroke-width=\"1\"/>\n"
    ));
    svg.push_str(&format!(
        "  <text x=\"{}\" y=\"35\" text-anchor=\"middle\" class=\"pill\">{}</text>\n",
        pill_x + pill_w / 2,
        xml_escape(&badge_text)
    ));

    // Header divider
    svg.push_str(&format!(
        "  <line x1=\"24\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{divider}\" stroke-width=\"1\"/>\n",
        header_h - 8,
        width - 24,
        header_h - 8
    ));

    // Rows
    for (idx, (label, subtitle, val, gain, is_baseline, p_val)) in rows.iter().enumerate() {
        let y = header_h + idx * row_h;
        let frac = (val / max_val).clamp(0.03, 1.0);
        let fill_w = (bar_max_w * frac).round().max(8.0) as usize;
        let fill_color = if *is_baseline { bar_base } else { bar_accent };

        svg.push_str(&format!(
            "  <text x=\"24\" y=\"{}\" class=\"label\">{}</text>\n",
            y + 18,
            xml_escape(label)
        ));
        svg.push_str(&format!(
            "  <text x=\"24\" y=\"{}\" class=\"meta\">{}</text>\n",
            y + 33,
            xml_escape(subtitle)
        ));

        // Bar track + bar fill
        svg.push_str(&format!(
            "  <rect x=\"{bar_x}\" y=\"{}\" width=\"{}\" height=\"14\" rx=\"4\" fill=\"{track_bg}\"/>\n",
            y + 13,
            bar_max_w as usize
        ));
        svg.push_str(&format!(
            "  <rect x=\"{bar_x}\" y=\"{}\" width=\"{fill_w}\" height=\"14\" rx=\"4\" fill=\"{fill_color}\"/>\n",
            y + 13
        ));

        // Value + gain on right
        let val_str = format_metric_value(*val, data.metric);
        let gain_str = if *is_baseline {
            "1.00x".to_string()
        } else {
            format!("{gain:.2}x")
        };
        let p_str = match p_val {
            Some(p) => format!("p={p:.4}"),
            None if *is_baseline => "baseline".to_string(),
            None => "verified".to_string(),
        };

        svg.push_str(&format!(
            "  <text x=\"656\" y=\"{}\" text-anchor=\"end\" class=\"{}\">{}</text>\n",
            y + 24,
            if *is_baseline { "meta" } else { "gain" },
            xml_escape(&gain_str)
        ));
        svg.push_str(&format!(
            "  <text x=\"{}\" y=\"{}\" text-anchor=\"end\" class=\"val\">{}</text>\n",
            width - 24,
            y + 18,
            xml_escape(&val_str)
        ));
        svg.push_str(&format!(
            "  <text x=\"{}\" y=\"{}\" text-anchor=\"end\" class=\"meta\">{}</text>\n",
            width - 24,
            y + 33,
            xml_escape(&p_str)
        ));
    }

    // Footer divider & text
    let footer_y = header_h + rows.len() * row_h + 6;
    svg.push_str(&format!(
        "  <line x1=\"24\" y1=\"{footer_y}\" x2=\"{}\" y2=\"{footer_y}\" stroke=\"{divider}\" stroke-width=\"1\"/>\n",
        width - 24
    ));
    svg.push_str(&format!(
        "  <text x=\"24\" y=\"{}\" class=\"meta\">{}</text>\n",
        footer_y + 24,
        xml_escape(&footer_left)
    ));
    svg.push_str(&format!(
        "  <text x=\"{}\" y=\"{}\" text-anchor=\"end\" class=\"meta\">generated by oa report --format svg</text>\n",
        width - 24,
        footer_y + 24
    ));

    svg.push_str("</svg>\n");
    svg
}

/// Renders a self-contained dark or light SVG chart for a standalone `oa compare` run.
pub fn render_compare_svg(res: &CompareResult, theme: SvgTheme) -> String {
    let title = res
        .bench_cmd
        .as_deref()
        .map(|b| format!("oa compare: {}", truncate_chars(b, 44)))
        .unwrap_or_else(|| "oa compare".to_string());
    let sha_prefix = res.stdout_sha256_a.chars().take(12).collect::<String>();
    let data = SvgChartData {
        repo_name: title,
        metric: res.metric,
        baseline_value: res.stats.baseline.median_ms,
        best_value: res.stats.candidate.median_ms,
        cumulative_gain: res.stats.paired_ratio_median,
        rss_before_kb: res.peak_rss_a_kb,
        rss_after_kb: res.peak_rss_b_kb,
        sha256_prefix: if res.outputs_identical {
            sha_prefix
        } else {
            "differs".to_string()
        },
        locked_files: 0,
        kept_count: res.stats.baseline.count,
        reverted_count: 0,
        steps: vec![SvgStep {
            label: truncate_chars(&res.cmd_b, 28),
            subtitle: format!(
                "95% CI [{:.2}x, {:.2}x] | n={}",
                res.stats.ci95_low, res.stats.ci95_high, res.stats.baseline.count
            ),
            value: res.stats.candidate.median_ms,
            gain: res.stats.paired_ratio_median,
            p_value: Some(res.stats.mann_whitney_p),
        }],
    };
    let rendered = render_session_svg(&data, theme);
    rendered
        .replace(
            "refs/oa/baseline",
            &xml_escape(&truncate_chars(&res.cmd_a, 32)),
        )
        .replace(
            "generated by oa report --format svg",
            "generated by oa compare --svg",
        )
}
