//! Hosted projects (web spec §3.1): a project published on a static site beside the web
//! editor, which the page copies into browser storage when its URL names it.
//!
//! `schemify publish` writes `<site>/projects/<name>/` with every project file, its saved
//! runs, and the manifest [`MANIFEST`]: one `<sha256 hex>  <path>` line per file, sorted by
//! path. The page opened on `#/projects/<name>/…` fetches the manifest from its own origin;
//! when the text differs from the copy it stored last time ([`STAMP`]), it replaces the
//! project's files in storage with the listed ones, each checked against its hash. An
//! unchanged manifest leaves local edits alone.
//!
//! Pure: no I/O, so both sides (native publish, web fetch) share one parser and one set of
//! checks, tested natively.

use schemify_doc::{hex, sha256};

/// The manifest's file name in a published project directory.
pub const MANIFEST: &str = "schemify.files";
/// Where the page keeps the manifest it last copied, relative to the project (hidden, so
/// it stays out of listings and exports).
pub const STAMP: &str = ".schemify/hosted";
/// Files one hosted project may list (the import limit, web spec §3).
pub const FILES_MAX: usize = 1 << 16;

/// The manifest text for `files` (`(relative path, bytes)`), sorted by path.
///
/// # Panics
///
/// Panics if a path is unsafe ([`safe_path`]) or listed twice.
pub fn manifest(files: &[(String, Vec<u8>)]) -> String {
    let mut rows: Vec<(&str, String)> = files
        .iter()
        .map(|(p, b)| {
            assert!(safe_path(p), "{p:?}: unsafe path");
            (p.as_str(), hex(&sha256(b)))
        })
        .collect();
    rows.sort_unstable();
    assert!(
        rows.windows(2).all(|w| w[0].0 != w[1].0),
        "a path listed twice"
    );
    let mut out = String::new();
    for (p, h) in rows {
        out.push_str(&h);
        out.push_str("  ");
        out.push_str(p);
        out.push('\n');
    }
    out
}

/// The `(relative path, hex digest)` rows of manifest `text`.
///
/// # Errors
///
/// A line that is not `<64 lowercase hex>  <path>`, an unsafe or repeated path, or more
/// than [`FILES_MAX`] rows: the whole manifest is refused (a trust boundary: nothing is
/// written).
pub fn parse(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
        if rows.len() >= FILES_MAX {
            return Err(format!("more than {FILES_MAX} files"));
        }
        let bad = || format!("{MANIFEST}:{}: not `<sha256>  <path>`", n + 1);
        let (h, p) = line.split_once("  ").ok_or_else(bad)?;
        let hex_ok = h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !hex_ok {
            return Err(bad());
        }
        if !safe_path(p) {
            return Err(format!("{MANIFEST}:{}: {p:?}: unsafe path", n + 1));
        }
        rows.push((p.to_owned(), h.to_owned()));
    }
    let mut names: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    names.sort_unstable();
    if names.windows(2).any(|w| w[0] == w[1]) {
        return Err(format!("{MANIFEST}: a path listed twice"));
    }
    assert!(rows.len() <= FILES_MAX);
    Ok(rows)
}

/// Whether `bytes` are the file a manifest row lists with digest `hex_digest`.
pub fn matches(hex_digest: &str, bytes: &[u8]) -> bool {
    hex(&sha256(bytes)) == hex_digest
}

/// A relative path with no empty, `.` or `..` segment and no `\`, `:` or NUL (the zip
/// import's rule, web spec §3).
pub fn safe_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == ".." || s.contains(['\\', ':', '\0']))
}

/// The project a page URL argument names (`/projects/<name>` or a path below it), when the
/// name is one a project directory can have: `A–Z a–z 0–9 . _ -`, not starting with `.`,
/// at most 64 characters.
pub fn project_name(arg: &str) -> Option<&str> {
    let rest = arg.strip_prefix("/projects/")?;
    let name = rest.split('/').next()?;
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    ok.then_some(name)
}

/// One published bench on the index page: its path in the project and the jobs of its
/// newest saved run (`None` when it was never simulated).
pub struct Bench {
    /// Path relative to the project root (`lib/tb.shbench`).
    pub path: String,
    /// `(done, failed)` job counts of the newest run.
    pub jobs: Option<(u32, u32)>,
}

/// The page `schemify publish` writes at `<site>/projects/index.html`: each project with a
/// link per bench into the editor one directory up (`../#/projects/<name>/<bench>`), and
/// how its newest run went. Plain HTML, no script: the editor does the work.
pub fn index_html(projects: &[(String, Vec<Bench>)]) -> String {
    let esc = |t: &str| {
        t.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let mut body = String::new();
    for (name, benches) in projects {
        assert!(
            project_name(&format!("/projects/{name}")).is_some(),
            "{name}"
        );
        body.push_str(&format!(
            "<h2><a href=\"../#/projects/{n}\">{n}</a></h2>\n<ul>\n",
            n = esc(name)
        ));
        if benches.is_empty() {
            body.push_str("<li>no benches</li>\n");
        }
        for b in benches {
            let status = match b.jobs {
                None => "not simulated".to_owned(),
                Some((done, 0)) => format!("{done} job{} done", if done == 1 { "" } else { "s" }),
                Some((done, failed)) => format!("{failed} of {} jobs failed", done + failed),
            };
            let class = match b.jobs {
                Some((_, 0)) => "ok",
                Some(_) => "fail",
                None => "none",
            };
            body.push_str(&format!(
                "<li><a href=\"../#/projects/{n}/{p}\">{p}</a> <span class=\"{class}\">{status}</span></li>\n",
                n = esc(name),
                p = esc(&b.path),
            ));
        }
        body.push_str("</ul>\n");
    }
    format!("{INDEX_HEAD}{body}</body>\n</html>\n")
}

const INDEX_HEAD: &str = "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>Schemify projects</title>
<style>
:root { color-scheme: light dark; --bg: #fafafa; --fg: #1a1a1a; --mute: #666; --ok: #15803d; --fail: #b91c1c; --link: #2563eb; }
@media (prefers-color-scheme: dark) { :root { --bg: #111; --fg: #eee; --mute: #999; --ok: #4ade80; --fail: #f87171; --link: #60a5fa; } }
body { margin: 0 auto; max-width: 720px; padding: 32px 16px; background: var(--bg); color: var(--fg); font: 16px/1.5 system-ui, sans-serif; }
a { color: var(--link); } h2 { font-size: 1.15rem; margin: 24px 0 4px; } ul { margin: 0; padding-left: 20px; }
span { font-size: .9rem; margin-left: 8px; } .ok { color: var(--ok); } .fail { color: var(--fail); } .none { color: var(--mute); }
</style>
</head>
<body>
<h1>Schemify projects</h1>
<p>Opening a project copies it into this browser's storage; your edits stay there until the
site publishes a new version of it.</p>
";

#[cfg(test)]
mod tests {
    use super::*;

    fn files(rows: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        rows.iter()
            .map(|(p, b)| ((*p).to_owned(), b.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn manifest_round_trips_sorted_and_checks_bytes() {
        let f = files(&[
            ("lib/b.shcomp", "b"),
            (".schemify/runs/x/results", "r"),
            ("a.shbench", "a"),
        ]);
        let text = manifest(&f);
        let rows = parse(&text).unwrap();
        let paths: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            paths,
            [".schemify/runs/x/results", "a.shbench", "lib/b.shcomp"]
        );
        for (p, h) in &rows {
            let bytes = &f.iter().find(|x| &x.0 == p).unwrap().1;
            assert!(matches(h, bytes));
            assert!(!matches(h, b"tampered"));
        }
        assert_eq!(manifest(&f), text, "deterministic");
    }

    #[test]
    fn parse_refuses_unsafe_or_malformed_manifests() {
        let h = "0".repeat(64);
        for bad in [
            format!("{h}  ../escape"),
            format!("{h}  /abs"),
            format!("{h}  a//b"),
            format!("{h}  a/./b"),
            format!("{h}  c:\\x"),
            format!("{}  short", "0".repeat(63)),
            format!("{}  upper", "A".repeat(64)),
            format!("{h} one-space"),
            format!("{h}  dup\n{h}  dup"),
        ] {
            assert!(parse(&bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse(&format!("{h}  ok/file\n\n")).unwrap().len(), 1);
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn index_links_each_bench_and_escapes_paths() {
        let html = index_html(&[(
            "demo".to_owned(),
            vec![
                Bench {
                    path: "tb/a.shbench".into(),
                    jobs: Some((2, 0)),
                },
                Bench {
                    path: "tb/<b>.shbench".into(),
                    jobs: Some((1, 1)),
                },
                Bench {
                    path: "tb/c.shbench".into(),
                    jobs: None,
                },
            ],
        )]);
        assert!(html.contains("href=\"../#/projects/demo/tb/a.shbench\""));
        assert!(html.contains("2 jobs done"));
        assert!(html.contains("1 of 2 jobs failed"));
        assert!(html.contains("not simulated"));
        assert!(html.contains("tb/&lt;b&gt;.shbench") && !html.contains("<b>"));
        assert!(!html.contains("<script"));
    }

    #[test]
    fn project_name_takes_only_plain_names() {
        assert_eq!(project_name("/projects/demo"), Some("demo"));
        assert_eq!(project_name("/projects/demo/tb/x.shbench"), Some("demo"));
        assert_eq!(project_name("/projects/a.b_c-1/"), Some("a.b_c-1"));
        for bad in [
            "/projects/",
            "/projects/..",
            "/projects/.hidden",
            "/projects/a b",
            "/other/x",
            "projects/x",
        ] {
            assert_eq!(project_name(bad), None, "{bad}");
        }
        assert_eq!(project_name(&format!("/projects/{}", "a".repeat(65))), None);
    }
}
