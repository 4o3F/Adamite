use super::{Case, TestResult};
use crate::{
    Entry, JobProgress, JobState, Launcher, ScanJob, SnapshotInfo, Sources, TuiError,
    app::{Action, App},
    launcher_view,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers},
};
use std::{
    cell::Cell,
    collections::VecDeque,
    io,
    rc::Rc,
    time::{Duration, Instant, SystemTime},
};

struct FakeJob {
    states: VecDeque<JobState>,
    drops: Rc<Cell<usize>>,
}

impl ScanJob for FakeJob {
    fn poll(&mut self) -> JobState {
        self.states.pop_front().unwrap_or_else(|| running(None))
    }
}

impl Drop for FakeJob {
    fn drop(&mut self) {
        self.drops.set(self.drops.get().saturating_add(1));
    }
}

#[derive(Default)]
struct FakeSources {
    rows: Vec<Entry>,
    jobs: VecDeque<VecDeque<JobState>>,
    starts: Vec<(Entry, bool)>,
    paths: Vec<String>,
    listings: usize,
    drops: Rc<Cell<usize>>,
}

impl FakeSources {
    fn job(&mut self) -> Box<dyn ScanJob> {
        Box::new(FakeJob {
            states: self.jobs.pop_front().unwrap_or_default(),
            drops: Rc::clone(&self.drops),
        })
    }
}

impl Sources for FakeSources {
    fn entries(&mut self) -> Vec<Entry> {
        self.listings = self.listings.saturating_add(1);
        self.rows.clone()
    }

    fn start(&mut self, entry: &Entry, rescan: bool) -> Box<dyn ScanJob> {
        self.starts.push((entry.clone(), rescan));
        self.job()
    }

    fn open_path(&mut self, path: &str) -> Box<dyn ScanJob> {
        self.paths.push(path.to_owned());
        self.job()
    }
}

fn info(name: &str, seconds: u64, error: Option<&str>) -> SnapshotInfo {
    SnapshotInfo {
        name: name.to_owned(),
        path: format!("/snapshots/{name}.adm"),
        modified: SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds)),
        error: error.map(str::to_owned),
    }
}

fn rows() -> Vec<Entry> {
    vec![
        Entry::Drive {
            letter: 'C',
            label: "System".into(),
            capacity: 1 << 40,
            snapshot: Some(info("C", 0, None)),
        },
        Entry::Drive {
            letter: 'D',
            label: "Data\nvolume".into(),
            capacity: 1 << 30,
            snapshot: None,
        },
        Entry::Snapshot(info("archive", 0, None)),
        Entry::Snapshot(info("broken", 0, Some("cannot read metadata"))),
    ]
}

fn running(progress: Option<JobProgress>) -> JobState {
    JobState::Running {
        label: "Scanning C:".into(),
        progress,
    }
}

fn progress(records: u64, planned: Option<u64>, bytes: u64) -> JobProgress {
    JobProgress {
        phase: "reading MFT".into(),
        records,
        planned,
        bytes,
    }
}

fn key(code: KeyCode) -> event::Event {
    event::Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn press(launcher: &mut Launcher<FakeSources>, code: KeyCode, now: Instant) -> Action {
    launcher.handle_terminal(key(code), now)
}

fn text(launcher: &Launcher<FakeSources>, width: u16, height: u16) -> TestResult<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(45))
        .ok_or("clock overflow")?;
    terminal.draw(|frame| launcher_view::render(launcher, frame, now))?;
    Ok(Case::lines(terminal.backend()).join("\n"))
}

#[test]
fn home_renders_drives_snapshots_errors_and_the_fixed_last_row() -> TestResult {
    let mut launcher = Launcher::new(FakeSources {
        rows: rows(),
        ..FakeSources::default()
    });
    launcher.handle_terminal(event::Event::Resize(100, 16), Instant::now());
    let rendered = text(&launcher, 100, 16)?;
    for expected in [
        "Adamite — Home",
        "C:  System",
        "TiB",
        "snapshot 45s ago",
        "D:  Data\\u{a}volume",
        "not scanned",
        "archive",
        "broken",
        "cannot read metadata",
        "Open file…",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?}: {rendered}"
        );
    }
    assert!(!rendered.contains("No saved snapshots"));
    let empty = Launcher::new(FakeSources::default());
    let rendered = text(&empty, 80, 12)?;
    assert!(rendered.contains("No saved snapshots"));
    assert!(rendered.contains("Open file…"));
    assert!(text(&empty, 39, 7)?.contains("terminal too small"));
    Ok(())
}

#[test]
fn enter_and_rescan_start_the_selected_row_with_the_right_flag() {
    let now = Instant::now();
    for (index, code, rescan) in [
        (0, KeyCode::Enter, false),
        (0, KeyCode::Char('r'), true),
        (1, KeyCode::Enter, false),
        (1, KeyCode::Char('r'), true),
        (2, KeyCode::Enter, false),
    ] {
        let mut launcher = Launcher::new(FakeSources {
            rows: rows(),
            ..FakeSources::default()
        });
        for _ in 0..index {
            press(&mut launcher, KeyCode::Down, now);
        }
        let selected = launcher.home.entries.get(index).cloned();
        assert_eq!(press(&mut launcher, code, now), Action::Continue);
        assert_eq!(
            launcher.sources.starts,
            selected
                .into_iter()
                .map(|entry| (entry, rescan))
                .collect::<Vec<_>>()
        );
        assert!(launcher.active.is_some());
    }
    let mut launcher = Launcher::new(FakeSources {
        rows: rows(),
        ..FakeSources::default()
    });
    press(&mut launcher, KeyCode::End, now);
    press(&mut launcher, KeyCode::Up, now);
    press(&mut launcher, KeyCode::Char('r'), now);
    assert!(launcher.sources.starts.is_empty());
}

#[test]
fn home_input_reuses_editing_and_keeps_quit_keys_as_text() -> TestResult {
    let mut launcher = Launcher::new(FakeSources::default());
    let now = Instant::now();
    press(&mut launcher, KeyCode::Enter, now);
    for c in "q1o.adm".chars() {
        assert_eq!(
            press(&mut launcher, KeyCode::Char(c), now),
            Action::Continue
        );
    }
    press(&mut launcher, KeyCode::Home, now);
    press(&mut launcher, KeyCode::Right, now);
    press(&mut launcher, KeyCode::Delete, now);
    press(&mut launcher, KeyCode::End, now);
    assert!(text(&launcher, 80, 12)?.contains("Open file: qo.adm"));
    press(&mut launcher, KeyCode::Enter, now);
    assert_eq!(launcher.sources.paths, ["qo.adm"]);
    assert!(launcher.active.is_some());
    let mut launcher = Launcher::new(FakeSources::default());
    press(&mut launcher, KeyCode::Enter, now);
    press(&mut launcher, KeyCode::Char('q'), now);
    press(&mut launcher, KeyCode::Esc, now);
    assert!(launcher.home.input.is_none());
    assert!(launcher.sources.paths.is_empty());
    assert_eq!(press(&mut launcher, KeyCode::Char('q'), now), Action::Quit);
    Ok(())
}

#[test]
fn home_navigation_help_and_resize_share_browser_cursor_rules() -> TestResult {
    let mut launcher = Launcher::new(FakeSources {
        rows: (0..20)
            .map(|index| Entry::Snapshot(info(&format!("snapshot-{index}"), 0, None)))
            .collect(),
        ..FakeSources::default()
    });
    let now = Instant::now();
    launcher.handle_terminal(event::Event::Resize(80, 8), now);
    for (code, selected) in [
        (KeyCode::PageDown, 4),
        (KeyCode::Down, 5),
        (KeyCode::PageUp, 1),
        (KeyCode::End, 20),
        (KeyCode::Up, 19),
        (KeyCode::Home, 0),
    ] {
        press(&mut launcher, code, now);
        assert_eq!(launcher.home.cursor.selected, selected);
        Case::assert_cursor(launcher.home.cursor, 21, 4);
    }
    press(&mut launcher, KeyCode::Char('?'), now);
    let rendered = text(&launcher, 80, 16)?;
    assert!(rendered.contains("rescan selected drive"));
    press(&mut launcher, KeyCode::End, now);
    press(&mut launcher, KeyCode::Enter, now);
    assert_eq!(launcher.home.cursor.selected, 0);
    assert!(launcher.active.is_none());
    press(&mut launcher, KeyCode::Esc, now);
    assert!(!launcher.home.help);
    press(&mut launcher, KeyCode::Char('?'), now);
    assert_eq!(press(&mut launcher, KeyCode::Char('q'), now), Action::Quit);
    Ok(())
}

#[test]
fn progress_renders_known_and_unknown_totals_and_checked_rates() -> TestResult {
    let now = Instant::now();
    let later = now
        .checked_add(Duration::from_secs(2))
        .ok_or("clock overflow")?;
    for (planned, expected) in [
        (Some(100), "50 / 100 records"),
        (None, "50 records · total unknown"),
        (Some(0), "50 / 0 records"),
    ] {
        let mut launcher = Launcher::new(FakeSources {
            rows: rows(),
            jobs: [VecDeque::from([
                running(Some(progress(50, planned, 4096))),
                running(None),
            ])]
            .into(),
            ..FakeSources::default()
        });
        press(&mut launcher, KeyCode::Enter, now);
        assert!(launcher.poll(later).is_none());
        let rendered = text(&launcher, 80, 12)?;
        for expected in [
            "Scanning C:",
            "reading MFT",
            expected,
            "Bytes read: 4.0 KiB",
            "Rate: 2.0 KiB/s",
        ] {
            assert!(rendered.contains(expected), "{rendered}");
        }
        assert!(launcher.poll(later).is_none());
        assert!(text(&launcher, 80, 12)?.contains("reading MFT"));
        assert_eq!(press(&mut launcher, KeyCode::Esc, now), Action::Continue);
        assert!(launcher.active.is_some());
    }
    let mut launcher = Launcher::new(FakeSources {
        rows: rows(),
        jobs: [VecDeque::from([running(Some(progress(
            u64::MAX,
            Some(1),
            u64::MAX,
        )))])]
        .into(),
        ..FakeSources::default()
    });
    press(&mut launcher, KeyCode::Enter, now);
    launcher.poll(now);
    assert!(text(&launcher, 80, 8)?.contains("Rate: 0 B/s"));
    let mut launcher = Launcher::new(FakeSources {
        rows: rows(),
        ..FakeSources::default()
    });
    press(&mut launcher, KeyCode::Enter, now);
    launcher.poll(now);
    assert!(text(&launcher, 80, 8)?.contains("Working… · total unknown"));
    Ok(())
}

fn run_script(
    launcher: &mut Launcher<FakeSources>,
    script: Vec<(Option<Duration>, Option<KeyCode>)>,
) -> TestResult<String> {
    let mut script = VecDeque::from(script);
    let mut terminal = Terminal::new(TestBackend::new(100, 16))?;
    launcher.run_loop(&mut terminal, &mut |timeout| {
        let (expected_timeout, event) = script.pop_front().ok_or_else(|| TuiError::Io {
            operation: "read scripted event",
            source: io::Error::other("event script exhausted"),
        })?;
        assert_eq!(timeout, expected_timeout);
        Ok(event.map(key))
    })?;
    assert!(script.is_empty());
    Ok(Case::lines(terminal.backend()).join("\n"))
}

#[test]
fn running_done_opens_tree_and_q_quits_from_the_browser() -> TestResult {
    let output = Case::fixture("basic-4k")?.output;
    let mut launcher = Launcher::new(FakeSources {
        rows: rows(),
        jobs: [VecDeque::from([running(None), JobState::Done(output)])].into(),
        ..FakeSources::default()
    });
    let rendered = run_script(
        &mut launcher,
        vec![
            (None, Some(KeyCode::Enter)),
            (Some(Duration::from_millis(100)), None),
            (None, Some(KeyCode::Char('q'))),
        ],
    )?;
    assert!(
        rendered.contains("Tree") && rendered.contains("bigdir"),
        "{rendered}"
    );
    assert_eq!(launcher.sources.drops.get(), 1);
    assert_eq!(launcher.sources.listings, 1);
    Ok(())
}

#[test]
fn browser_o_returns_home_refreshes_entries_and_does_not_affect_standalone_browser() -> TestResult {
    let output = Case::fixture("basic-4k")?.output;
    let mut standalone = App::new(&output, adamite_core::snapshot::NodeId::ROOT, 8);
    assert_eq!(
        standalone.handle_terminal(key(KeyCode::Char('o'))),
        Action::Continue
    );
    let mut launcher = Launcher::new(FakeSources {
        rows: rows(),
        jobs: [VecDeque::from([JobState::Done(output)])].into(),
        ..FakeSources::default()
    });
    launcher.sources.rows = vec![Entry::Snapshot(info("refreshed snapshot", 0, None))];
    let rendered = run_script(
        &mut launcher,
        vec![
            (None, Some(KeyCode::Enter)),
            (None, Some(KeyCode::Char('o'))),
            (None, Some(KeyCode::Char('q'))),
        ],
    )?;
    assert!(
        rendered.contains("Adamite — Home") && rendered.contains("refreshed snapshot"),
        "{rendered}"
    );
    assert_eq!(launcher.sources.listings, 2);
    Ok(())
}

#[test]
fn running_failed_returns_home_with_the_child_message() -> TestResult {
    let mut launcher = Launcher::new(FakeSources {
        rows: rows(),
        jobs: [VecDeque::from([
            running(None),
            JobState::Failed("elevation was declined".into()),
        ])]
        .into(),
        ..FakeSources::default()
    });
    let rendered = run_script(
        &mut launcher,
        vec![
            (None, Some(KeyCode::Enter)),
            (Some(Duration::from_millis(100)), None),
            (None, Some(KeyCode::Char('q'))),
        ],
    )?;
    assert!(
        rendered.contains("Adamite — Home") && rendered.contains("elevation was declined"),
        "{rendered}"
    );
    assert_eq!(launcher.sources.listings, 2);
    assert_eq!(launcher.sources.drops.get(), 1);
    Ok(())
}

#[test]
fn q_quits_home_and_progress_and_drops_the_running_job() -> TestResult {
    let mut launcher = Launcher::new(FakeSources {
        rows: rows(),
        ..FakeSources::default()
    });
    run_script(&mut launcher, vec![(None, Some(KeyCode::Char('q')))])?;
    assert_eq!(launcher.sources.drops.get(), 0);
    let rendered = run_script(
        &mut launcher,
        vec![
            (None, Some(KeyCode::Enter)),
            (Some(Duration::from_millis(100)), Some(KeyCode::Esc)),
            (Some(Duration::from_millis(100)), Some(KeyCode::Char('q'))),
        ],
    )?;
    assert!(rendered.contains("Scanning C:"));
    assert_eq!(launcher.sources.drops.get(), 1);
    assert!(launcher.active.is_none());
    Ok(())
}
