use crate::{
    app::{Action, App, Cursor, Tab},
    input::{Event, Key},
    view::View,
};
use adamite_core::{
    scan::{ScanConfig, ScanContext, ScanOutput},
    snapshot::{NodeFlags, NodeId},
};
use adamite_fixtures::{
    Fixture,
    builders::{AttrBuilder, BootSectorBuilder, FileNameBuilder, NonResidentModel, RecordBuilder},
    runlist_encoder::{RunModel, RunlistEncoder},
};
use adamite_io::{driver::SyncDriver, reader::SliceReader};
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{self, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    style::{Color, Modifier},
};
use std::{error::Error, path::Path};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Case {
    output: ScanOutput,
}

impl Case {
    const LONG_NAME: &'static str = "long-漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字-tail";

    fn fixture(name: &str) -> TestResult<Self> {
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../tests/fixtures/ntfs/{name}.json")),
        )?;
        Self::scan(fixture.image())
    }

    fn scan(image: &[u8]) -> TestResult<Self> {
        let mut reader = SliceReader::new(image);
        let output = SyncDriver::new(ScanConfig {
            context: ScanContext {
                label: Some("test\nvolume".to_owned()),
                scanned_at_filetime: Some(116_444_736_012_345_678),
                free_bytes: Some(4096),
            },
            ..ScanConfig::default()
        })?
        .run(&mut reader)?;
        Ok(Self { output })
    }

    fn id(&self, path: &str) -> TestResult<NodeId> {
        for index in 0..self.output.snapshot.len() {
            let id = NodeId::from_raw(u32::try_from(index)?);
            if self.output.snapshot.path(id)?.as_deref() == Some(path) {
                return Ok(id);
            }
        }
        Err(format!("missing test path {path:?}").into())
    }

    fn app(&self, path: &str, page_rows: usize) -> TestResult<App<'_>> {
        Ok(App::new(&self.output, self.id(path)?, page_rows))
    }

    fn select(app: &mut App<'_>, id: NodeId) -> TestResult {
        let index = app
            .children()
            .iter()
            .position(|&child| child == id)
            .ok_or("node is not a child of the current directory")?;
        app.handle(Event::Key(Key::Home));
        for _ in 0..index {
            app.handle(Event::Key(Key::Down));
        }
        Ok(())
    }

    fn render(app: &App<'_>, width: u16, height: u16) -> TestResult<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        terminal.draw(|frame| View::new(app).render(frame))?;
        Ok(terminal.backend().clone())
    }

    fn lines(backend: &TestBackend) -> Vec<String> {
        backend
            .buffer()
            .content
            .chunks(usize::from(backend.buffer().area.width).max(1))
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn text(app: &App<'_>, width: u16, height: u16) -> TestResult<String> {
        Ok(Self::lines(&Self::render(app, width, height)?).join("\n"))
    }

    fn press(app: &mut App<'_>, code: KeyCode) -> TestResult<Action> {
        let event =
            Event::from_terminal(event::Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                .ok_or("unmapped test key")?;
        Ok(app.handle(event))
    }

    fn select_top(app: &mut App<'_>, id: NodeId) -> TestResult {
        Self::press(app, KeyCode::Char('2'))?;
        let index = app
            .snapshot()
            .top_files()
            .iter()
            .position(|&file| file == id)
            .ok_or("file missing from top index")?;
        app.handle(Event::Key(Key::Home));
        for _ in 0..index {
            app.handle(Event::Key(Key::Down));
        }
        Ok(())
    }

    fn selected_row(app: &App<'_>, width: u16, height: u16) -> TestResult<String> {
        let backend = Self::render(app, width, height)?;
        let lines = Self::lines(&backend);
        let row = (0..height)
            .find(|&y| {
                backend
                    .buffer()
                    .cell((0, y))
                    .is_some_and(|cell| cell.bg == Color::DarkGray)
            })
            .ok_or("selected row is not visible")?;
        lines
            .get(usize::from(row))
            .cloned()
            .ok_or_else(|| "selected row".into())
    }

    fn assert_cursor(cursor: Cursor, count: usize, page_rows: usize) {
        assert!(cursor.selected < count.max(1), "{cursor:?}, count {count}");
        assert!(cursor.scroll <= cursor.selected, "{cursor:?}");
        assert!(
            cursor.selected.saturating_sub(cursor.scroll) < page_rows,
            "{cursor:?}, page {page_rows}"
        );
        assert!(
            cursor.scroll <= count.saturating_sub(page_rows),
            "{cursor:?}, count {count}, page {page_rows}"
        );
    }

    fn name(parent: u32, text: &str) -> Vec<u8> {
        AttrBuilder::resident(
            0x30,
            FileNameBuilder {
                parent: (7 << 48) | u64::from(parent),
                namespace: 0,
                name: text.encode_utf16().collect(),
                ..FileNameBuilder::default()
            }
            .build(),
        )
        .build()
    }

    fn synthetic() -> TestResult<Self> {
        let mut image = vec![0; 128 * 512];
        image.get_mut(..512).ok_or("boot sector")?.copy_from_slice(
            &BootSectorBuilder {
                cluster_encoding: 1,
                sectors: 128,
                mirror: 64,
                ..BootSectorBuilder::default()
            }
            .build(),
        );
        let mft = AttrBuilder::non_resident(
            0x80,
            NonResidentModel {
                highest: 63,
                sizes: [32768; 4],
                mapping_pairs: RunlistEncoder {
                    runs: vec![RunModel {
                        lcn: Some(4),
                        len: 64,
                    }],
                }
                .encode(0),
                ..NonResidentModel::default()
            },
        )
        .build();
        let mut records = vec![
            RecordBuilder {
                record_number: 0,
                attributes: vec![mft, Self::name(5, "$MFT")],
                ..RecordBuilder::default()
            },
            RecordBuilder {
                record_number: 5,
                flags: 3,
                attributes: vec![Self::name(5, ".")],
                ..RecordBuilder::default()
            },
        ];
        for (number, parent, text, directory) in [
            (16, 5, "empty", true),
            (17, 5, "nested", true),
            (18, 17, "child", true),
            (19, 18, "leaf.txt", false),
            (20, 5, "zero", true),
            (21, 20, "resident.txt", false),
            (22, 5, "line\n\t\u{1b}.txt", false),
            (23, 5, Self::LONG_NAME, true),
            (24, 23, "tail-child", false),
            (25, 5, "reparse", true),
            (26, 5, "a.txt", false),
            (27, 5, "b.txt", false),
            (28, 5, "c.txt", false),
        ] {
            let mut attributes = vec![Self::name(parent, text)];
            if !directory {
                attributes.push(AttrBuilder::resident(0x80, b"hi".to_vec()).build());
            }
            if number == 25 {
                let mut value = vec![0; 8];
                value
                    .get_mut(..4)
                    .ok_or("reparse tag")?
                    .copy_from_slice(&0xa000_000cu32.to_le_bytes());
                attributes.push(AttrBuilder::resident(0xc0, value).build());
            }
            records.push(RecordBuilder {
                record_number: number,
                flags: if directory { 3 } else { 1 },
                attributes,
                ..RecordBuilder::default()
            });
        }
        for record in records {
            let start = usize::try_from(record.record_number)?
                .checked_mul(1024)
                .and_then(|offset| offset.checked_add(2048))
                .ok_or("record offset")?;
            let end = start.checked_add(1024).ok_or("record end")?;
            image
                .get_mut(start..end)
                .ok_or("record range")?
                .copy_from_slice(&record.build());
        }
        Self::scan(&image)
    }
}

#[test]
fn basic_tree_renders_totals_and_only_visible_rows() -> TestResult {
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/", 4)?;
    let backend = Case::render(&app, 120, 8)?;
    let text = Case::lines(&backend).join("\n");
    assert!(text.contains("Tree"), "{text}");
    assert!(text.contains("Allocated"), "{text}");
    assert!(text.contains("Share of parent"), "{text}");
    assert!(text.contains("<NTFS metadata>/"), "{text}");
    assert!(text.contains("2.7 MiB"), "{text}");
    assert!(text.contains('‰') && text.contains('█'), "{text}");
    let metadata_row = Case::lines(&backend)
        .iter()
        .position(|line| line.contains("<NTFS metadata>/"))
        .ok_or("metadata row")?;
    let dimmed = backend
        .buffer()
        .cell((0, u16::try_from(metadata_row)?))
        .is_some_and(|cell| cell.modifier.contains(Modifier::DIM));
    assert!(dimmed, "synthetic directory should be dimmed");

    app.handle(Event::Key(Key::Enter));
    app.handle(Event::Key(Key::End));
    let text = Case::lines(&Case::render(&app, 120, 8)?).join("\n");
    assert!(text.contains("/<NTFS metadata>"), "{text}");
    assert!(!text.contains("$MFT "), "offscreen first row: {text}");
    Ok(())
}

#[test]
fn nested_navigation_restores_selection_and_scroll_at_each_level() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/", 2)?;
    Case::select(&mut app, case.id("/nested")?)?;
    let root_cursor = app.tree.cursor;
    assert!(root_cursor.scroll > 0, "root selection must be scrolled");
    app.handle(Event::Key(Key::Enter));
    assert_eq!(app.tree.directory, case.id("/nested")?);
    Case::select(&mut app, case.id("/nested/child")?)?;
    let nested_cursor = app.tree.cursor;
    app.handle(Event::Key(Key::Enter));
    assert_eq!(app.tree.directory, case.id("/nested/child")?);
    app.handle(Event::Key(Key::Enter));
    assert_eq!(
        app.tree.directory,
        case.id("/nested/child")?,
        "file cannot be entered"
    );
    app.handle(Event::Key(Key::Parent));
    assert_eq!(app.tree.cursor, nested_cursor);
    app.handle(Event::Key(Key::Parent));
    assert_eq!(app.tree.directory, NodeId::ROOT);
    assert_eq!(app.tree.cursor, root_cursor);
    app.handle(Event::Key(Key::Parent));
    assert_eq!(app.tree.cursor, root_cursor, "parent at root is a no-op");
    Ok(())
}

#[test]
fn movement_pages_and_endpoints_stay_within_children() -> TestResult {
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/bigdir", 4)?;
    app.handle(Event::Key(Key::Up));
    assert_eq!(app.tree.cursor.selected, 0);
    app.handle(Event::Key(Key::PageDown));
    assert_eq!(app.tree.cursor.selected, 4);
    assert_eq!(app.tree.cursor.scroll, 1);
    app.handle(Event::Key(Key::PageUp));
    assert_eq!(app.tree.cursor.selected, 0);
    app.handle(Event::Key(Key::End));
    assert_eq!(
        app.tree.cursor.selected,
        app.children().len().saturating_sub(1)
    );
    let last = app.tree.cursor;
    app.handle(Event::Key(Key::Down));
    assert_eq!(app.tree.cursor, last);
    app.handle(Event::Key(Key::Home));
    assert_eq!(app.tree.cursor.selected, 0);
    assert_eq!(app.tree.cursor.scroll, 0);
    Ok(())
}

#[test]
fn parent_from_start_directory_selects_the_child_in_its_parent() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/nested/child", 2)?;
    app.handle(Event::Key(Key::Parent));
    assert_eq!(app.tree.directory, case.id("/nested")?);
    assert_eq!(
        app.children().get(app.tree.cursor.selected),
        Some(&case.id("/nested/child")?)
    );
    Ok(())
}

#[test]
fn empty_directory_and_zero_allocation_parent_render() -> TestResult {
    let case = Case::synthetic()?;
    let mut empty = case.app("/empty", 4)?;
    for key in [Key::End, Key::PageDown, Key::Up, Key::Enter] {
        empty.handle(Event::Key(key));
    }
    assert_eq!(empty.tree.cursor.selected, 0);
    assert_eq!(empty.tree.cursor.scroll, 0);
    let text = Case::lines(&Case::render(&empty, 120, 8)?).join("\n");
    assert!(text.contains("(empty)"), "{text}");
    let zero = case.app("/zero", 4)?;
    let text = Case::lines(&Case::render(&zero, 120, 8)?).join("\n");
    let row = text
        .lines()
        .find(|line| line.contains("resident.txt"))
        .ok_or("resident row")?;
    assert!(row.contains("— [          ]"), "{row}");
    assert!(!row.contains('‰') && !row.contains('█'), "{row}");
    Ok(())
}

#[test]
fn frag_growth_resident_children_have_empty_share_bars() -> TestResult {
    let case = Case::fixture("frag-512")?;
    let app = case.app("/growth", 4)?;
    assert_eq!(
        case.output
            .snapshot
            .node(app.tree.directory)
            .ok_or("growth node")?
            .total_alloc(),
        303104
    );
    let text = Case::lines(&Case::render(&app, 120, 8)?).join("\n");
    assert!(text.contains("0‰ [          ]"), "{text}");
    assert!(!text.contains('█'), "{text}");
    Ok(())
}

#[test]
fn duplicate_hardlink_is_marked_without_a_share_bar() -> TestResult {
    let case = Case::fixture("basic-4k")?;
    let snapshot = &case.output.snapshot;
    let duplicate = (0..snapshot.len())
        .filter_map(|index| u32::try_from(index).ok().map(NodeId::from_raw))
        .filter_map(|id| snapshot.node(id))
        .find(|node| node.flags().contains(NodeFlags::HARDLINK_DUP))
        .ok_or("fixture hardlink duplicate")?;
    let mut app = App::new(&case.output, duplicate.parent(), 4);
    Case::select(&mut app, duplicate.id())?;
    let backend = Case::render(&app, 120, 8)?;
    let lines = Case::lines(&backend);
    let name = snapshot.name(duplicate.id()).ok_or("duplicate name")?;
    let row = lines
        .iter()
        .find(|line| line.contains(&format!("{name}=")))
        .ok_or("duplicate row")?;
    assert!(row.contains('—'), "{row}");
    assert!(
        !row.contains('[') && !row.contains('█') && !row.contains('‰'),
        "{row}"
    );
    Ok(())
}

#[test]
fn names_are_escaped_truncated_and_reparse_markers_are_visible() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/", 4)?;
    Case::select(&mut app, case.id("/line\n\t\u{1b}.txt")?)?;
    let text = Case::lines(&Case::render(&app, 120, 8)?).join("\n");
    assert!(text.contains("line\\u{a}\\u{9}\\u{1b}.txt"), "{text}");
    Case::select(&mut app, case.id("/reparse")?)?;
    let text = Case::lines(&Case::render(&app, 120, 8)?).join("\n");
    assert!(text.contains("reparse/↪"), "{text}");
    let long_path = format!("/{}", Case::LONG_NAME);
    Case::select(&mut app, case.id(&long_path)?)?;
    let text = Case::lines(&Case::render(&app, 80, 8)?).join("\n");
    let row = text
        .lines()
        .find(|line| line.contains("long-"))
        .ok_or("long row")?;
    assert!(row.contains("…/"), "{row}");
    assert!(!row.contains("-tail"), "{row}");
    app.handle(Event::Key(Key::Enter));
    let text = Case::lines(&Case::render(&app, 40, 8)?).join("\n");
    let header = text.lines().next().ok_or("header")?;
    assert!(
        header.starts_with("/long-")
            && header.contains('…')
            && header.trim_end().ends_with("-tail"),
        "{header}"
    );
    Ok(())
}

#[test]
fn quit_keys_release_and_repeat_filtering() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/", 4)?;
    let bindings = [
        (KeyCode::Char('q'), KeyModifiers::NONE, Key::Quit),
        (KeyCode::Esc, KeyModifiers::NONE, Key::Escape),
        (KeyCode::Char('c'), KeyModifiers::CONTROL, Key::Quit),
        (KeyCode::Up, KeyModifiers::NONE, Key::Up),
        (KeyCode::Char('k'), KeyModifiers::NONE, Key::Up),
        (KeyCode::Down, KeyModifiers::NONE, Key::Down),
        (KeyCode::Char('j'), KeyModifiers::NONE, Key::Down),
        (KeyCode::PageUp, KeyModifiers::NONE, Key::PageUp),
        (KeyCode::PageDown, KeyModifiers::NONE, Key::PageDown),
        (KeyCode::Home, KeyModifiers::NONE, Key::Home),
        (KeyCode::End, KeyModifiers::NONE, Key::End),
        (KeyCode::Enter, KeyModifiers::NONE, Key::Enter),
        (KeyCode::Right, KeyModifiers::NONE, Key::Open),
        (KeyCode::Char('l'), KeyModifiers::NONE, Key::Open),
        (KeyCode::Left, KeyModifiers::NONE, Key::Parent),
        (KeyCode::Backspace, KeyModifiers::NONE, Key::Parent),
        (KeyCode::Char('h'), KeyModifiers::NONE, Key::Parent),
        (KeyCode::Tab, KeyModifiers::NONE, Key::NextTab),
        (
            KeyCode::Char('1'),
            KeyModifiers::NONE,
            Key::SelectTab(Tab::Tree),
        ),
        (
            KeyCode::Char('2'),
            KeyModifiers::NONE,
            Key::SelectTab(Tab::TopFiles),
        ),
        (
            KeyCode::Char('3'),
            KeyModifiers::NONE,
            Key::SelectTab(Tab::Extensions),
        ),
        (
            KeyCode::Char('4'),
            KeyModifiers::NONE,
            Key::SelectTab(Tab::Info),
        ),
        (KeyCode::Char('b'), KeyModifiers::NONE, Key::Bytes),
        (KeyCode::Char('g'), KeyModifiers::NONE, Key::Group),
        (KeyCode::Char('?'), KeyModifiers::SHIFT, Key::Help),
    ];
    for (code, modifiers, key) in bindings {
        let terminal_event =
            |kind| event::Event::Key(KeyEvent::new_with_kind(code, modifiers, kind));
        let press = Event::from_terminal(terminal_event(KeyEventKind::Press));
        assert_eq!(press, Some(Event::Key(key)), "{code:?} press");
        assert_eq!(
            Event::from_terminal(terminal_event(KeyEventKind::Release)),
            None,
            "{code:?} release"
        );
        let movement = matches!(
            key,
            Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Home | Key::End
        );
        assert_eq!(
            Event::from_terminal(terminal_event(KeyEventKind::Repeat)),
            movement.then_some(Event::Key(key)),
            "{code:?} repeat"
        );
        if key == Key::Quit {
            assert_eq!(app.handle(press.ok_or("press event")?), Action::Quit);
        }
    }
    assert_eq!(Event::from_terminal(event::Event::FocusGained), None);
    assert_eq!(
        Event::from_terminal(event::Event::Key(KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::ALT
        ))),
        None
    );
    Ok(())
}

#[test]
fn tabs_cycle_and_digits_preserve_independent_cursors_and_scroll() -> TestResult {
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/bigdir", 4)?;
    let mut saved = Vec::new();
    for tab in Tab::ALL {
        assert_eq!(app.tab, tab);
        for key in [Key::End, Key::Up] {
            app.handle(Event::Key(key));
        }
        let cursor = match tab {
            Tab::Tree => app.tree.cursor,
            Tab::TopFiles => app.top_files,
            Tab::Extensions => app.extensions,
            Tab::Info => app.info,
            Tab::Search => app.search,
        };
        if tab != Tab::Search {
            assert!(cursor.selected > 0, "{tab:?}: {cursor:?}");
        }
        let text = Case::text(&app, 120, 8)?;
        assert!(text.contains(tab.title()), "{text}");
        assert_eq!(app.input.is_some(), tab == Tab::Search, "{tab:?}");
        saved.push((tab, cursor));
        Case::press(&mut app, KeyCode::Tab)?;
    }
    assert_eq!(app.tab, Tab::Tree, "Tab wraps after Search");
    for (code, (tab, cursor)) in ['5', '4', '3', '2', '1']
        .into_iter()
        .zip(saved.into_iter().rev())
    {
        Case::press(&mut app, KeyCode::Char(code))?;
        assert_eq!(app.tab, tab);
        if tab == Tab::Search {
            assert!(app.input.is_some());
            Case::press(&mut app, KeyCode::Esc)?;
        }
        let retained = match tab {
            Tab::Tree => app.tree.cursor,
            Tab::TopFiles => app.top_files,
            Tab::Extensions => app.extensions,
            Tab::Info => app.info,
            Tab::Search => app.search,
        };
        assert_eq!(retained, cursor, "{tab:?}");
        if tab != Tab::Search {
            assert!(!Case::selected_row(&app, 120, 8)?.trim().is_empty());
        }
    }
    Ok(())
}

#[test]
fn top_file_jump_rebuilds_each_ancestor_and_selects_visible_file() -> TestResult {
    for (case, path) in [
        (Case::synthetic()?, "/nested/child/leaf.txt"),
        (Case::synthetic()?, "/a.txt"),
        (Case::fixture("basic-4k")?, "/bigdir/f049"),
    ] {
        // Start with an unrelated navigation stack so a jump must replace it.
        let mut app = case.app("/", 4)?;
        app.handle(Event::Key(Key::Enter));
        let file = case.id(path)?;
        Case::select_top(&mut app, file)?;
        let top_cursor = app.top_files;
        let name = app.snapshot().name(file).ok_or("file name")?;
        assert!(Case::selected_row(&app, 120, 8)?.contains(name));
        Case::press(&mut app, KeyCode::Right)?;
        assert_eq!(app.tab, Tab::TopFiles, "only Enter jumps from Top files");
        Case::press(&mut app, KeyCode::Enter)?;
        assert_eq!(app.tab, Tab::Tree);
        let mut child = file;
        let mut steps = 0usize;
        while child != NodeId::ROOT {
            let parent = app.snapshot().node(child).ok_or("child")?.parent();
            assert_eq!(app.tree.directory, parent);
            assert_eq!(app.children().get(app.tree.cursor.selected), Some(&child));
            let name = app.snapshot().name(child).ok_or("child name")?;
            assert!(Case::selected_row(&app, 120, 8)?.contains(name), "{name}");
            Case::assert_cursor(app.tree.cursor, app.children().len(), 4);
            Case::press(
                &mut app,
                if steps.is_multiple_of(2) {
                    KeyCode::Left
                } else {
                    KeyCode::Char('h')
                },
            )?;
            child = parent;
            steps = steps.checked_add(1).ok_or("ancestor steps")?;
        }
        Case::press(&mut app, KeyCode::Char('2'))?;
        assert_eq!(app.top_files, top_cursor);
    }
    Ok(())
}

#[test]
fn top_rows_follow_stored_order_escape_paths_and_truncate_in_the_middle() -> TestResult {
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/", 4)?;
    Case::press(&mut app, KeyCode::Char('2'))?;
    let text = Case::text(&app, 120, 8)?;
    let lines: Vec<_> = text.lines().skip(3).take(4).collect();
    for (line, &id) in lines.into_iter().zip(app.snapshot().top_files()) {
        let node = app.snapshot().node(id).ok_or("top node")?;
        let path = app.snapshot().path(id)?.ok_or("top path")?;
        assert!(line.contains(&path), "{line}");
        for size in [node.alloc(), node.size()] {
            let value = adamite_format::ByteSize {
                value: u128::from(size),
                raw_bytes: false,
            }
            .to_string();
            assert!(line.contains(&value), "{line}");
        }
    }
    let case = Case::synthetic()?;
    let mut app = case.app("/", 4)?;
    Case::select_top(&mut app, case.id("/line\n\t\u{1b}.txt")?)?;
    let row = Case::selected_row(&app, 120, 8)?;
    assert!(row.contains("/line\\u{a}\\u{9}\\u{1b}.txt"), "{row}");
    Case::select_top(
        &mut app,
        case.id(&format!("/{}/tail-child", Case::LONG_NAME))?,
    )?;
    let row = Case::selected_row(&app, 60, 8)?;
    assert!(
        row.starts_with("/long-") && row.contains('…') && row.contains("/tail-child"),
        "{row}"
    );
    Ok(())
}

#[test]
fn extensions_render_none_and_totals_in_stored_order() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/", 20)?;
    Case::press(&mut app, KeyCode::Char('3'))?;
    let text = Case::text(&app, 120, 24)?;
    assert!(text.contains("(none)") && text.contains("txt"), "{text}");
    for (line, stat) in text.lines().skip(3).zip(app.snapshot().ext_stats()) {
        let label = if stat.extension().is_empty() {
            "(none)"
        } else {
            stat.extension()
        };
        assert!(line.contains(label), "{line}");
        assert!(line.contains(&stat.count().to_string()), "{line}");
        for size in [stat.total_size(), stat.total_alloc()] {
            let value = adamite_format::ByteSize {
                value: u128::from(size),
                raw_bytes: false,
            }
            .to_string();
            assert!(line.contains(&value), "{line}");
        }
    }
    let directory = app.tree.directory;
    for key in [Key::Enter, Key::Parent] {
        app.handle(Event::Key(key));
    }
    assert_eq!(
        app.tree.directory, directory,
        "extension navigation does not affect Tree"
    );
    Ok(())
}

#[test]
fn info_renders_volume_time_tree_and_only_nonzero_counters() -> TestResult {
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/", 56)?;
    Case::press(&mut app, KeyCode::Char('4'))?;
    let text = Case::text(&app, 120, 60)?;
    assert!(text.contains("1970-01-01T00:00:01Z"), "{text}");
    for label in [
        "Volume serial",
        "Label",
        "Bytes per sector",
        "Cluster size",
        "Volume size",
        "Free space",
        "Nodes",
        "Files",
        "Logical size",
        "Allocated size",
    ] {
        assert!(text.contains(label), "missing {label}: {text}");
    }
    assert!(
        text.contains("test\\u{a}volume") && text.contains("4.0 KiB"),
        "{text}"
    );
    for (name, value) in case.output.stats.counters() {
        let row = text.lines().find(|line| line.starts_with(name));
        if value == 0 {
            assert!(row.is_none(), "zero counter {name}: {text}");
        } else {
            assert!(
                row.is_some_and(|line| line.contains(&value.to_string())),
                "{name} = {value}: {text}"
            );
        }
    }
    Ok(())
}

#[test]
fn bytes_toggle_updates_every_size_cell_across_tabs() -> TestResult {
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/", 56)?;
    for _ in 0..2 {
        Case::press(&mut app, KeyCode::Char('b'))?;
        for code in ['1', '2', '3', '4'] {
            Case::press(&mut app, KeyCode::Char(code))?;
            let text = Case::text(&app, 180, 60)?;
            if app.raw_bytes {
                assert!(
                    !text.contains(" B") && !text.contains("KiB") && !text.contains("MiB"),
                    "{text}"
                );
                let value = match app.tab {
                    Tab::Tree => app
                        .snapshot()
                        .node(*app.children().first().ok_or("child")?)
                        .ok_or("node")?
                        .total_alloc(),
                    Tab::TopFiles => app
                        .snapshot()
                        .node(*app.snapshot().top_files().first().ok_or("top")?)
                        .ok_or("node")?
                        .alloc(),
                    Tab::Extensions => app
                        .snapshot()
                        .ext_stats()
                        .first()
                        .ok_or("ext")?
                        .total_alloc(),
                    Tab::Info => app.snapshot().volume().geometry().total_bytes(),
                    Tab::Search => 0,
                };
                assert!(text.contains(&value.to_string()), "{text}");
            } else {
                assert!(text.contains("KiB") || text.contains("MiB"), "{text}");
            }
        }
    }
    Ok(())
}

#[test]
fn help_overlay_captures_keys_and_escape_closes_before_quitting() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/", 4)?;
    Case::press(&mut app, KeyCode::Char('?'))?;
    assert!(app.help);
    let text = Case::text(&app, 40, 8)?;
    for key in [
        "Help",
        "PgUp/PgDn",
        "Home/End",
        "Tab/1–5",
        "Enter/→/l",
        "←/h/Backspace",
        "b: raw bytes",
        "q/Ctrl-C",
        "?/Esc",
    ] {
        assert!(text.contains(key), "missing {key}: {text}");
    }
    for code in [
        KeyCode::Down,
        KeyCode::Enter,
        KeyCode::Char('b'),
        KeyCode::Char('2'),
        KeyCode::Tab,
    ] {
        assert_eq!(Case::press(&mut app, code)?, Action::Continue);
    }
    assert_eq!(app.tab, Tab::Tree);
    assert_eq!(app.tree.directory, NodeId::ROOT);
    assert_eq!(app.tree.cursor, Cursor::default());
    assert!(!app.raw_bytes);
    assert_eq!(Case::press(&mut app, KeyCode::Esc)?, Action::Continue);
    assert!(!app.help);
    assert!(!Case::text(&app, 120, 8)?.contains("close help"));
    assert_eq!(Case::press(&mut app, KeyCode::Esc)?, Action::Quit);
    for _ in 0..2 {
        Case::press(&mut app, KeyCode::Char('?'))?;
    }
    assert!(!app.help, "? toggles closed");
    Case::press(&mut app, KeyCode::Char('?'))?;
    assert_eq!(Case::press(&mut app, KeyCode::Char('q'))?, Action::Quit);
    let ctrl_c = Event::from_terminal(event::Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )))
    .ok_or("Ctrl-C")?;
    assert_eq!(app.handle(ctrl_c), Action::Quit);
    Ok(())
}

#[test]
fn resize_clamps_all_cursors_and_saved_tree_ancestors() -> TestResult {
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/", 30)?;
    for code in ['1', '2', '3', '4'] {
        Case::press(&mut app, KeyCode::Char(code))?;
        app.handle(Event::Key(Key::End));
    }
    // Resize while help is open and a different tab is active.
    Case::press(&mut app, KeyCode::Char('?'))?;
    let resize = Event::from_terminal(event::Event::Resize(120, 8)).ok_or("resize event")?;
    assert_eq!(
        resize,
        Event::Resize {
            width: 120,
            height: 8
        }
    );
    app.handle(resize);
    Case::press(&mut app, KeyCode::Esc)?;
    Case::assert_cursor(app.tree.cursor, app.children().len(), 4);
    Case::assert_cursor(app.top_files, app.snapshot().top_files().len(), 4);
    Case::assert_cursor(app.extensions, app.snapshot().ext_stats().len(), 4);
    Case::assert_cursor(app.info, app.info_fields().len(), 4);
    for code in ['1', '2', '3', '4'] {
        Case::press(&mut app, KeyCode::Char(code))?;
        assert!(!Case::selected_row(&app, 120, 8)?.trim().is_empty());
        app.handle(Event::Key(Key::Home));
        app.handle(Event::Key(Key::PageDown));
        let (cursor, count) = match app.tab {
            Tab::Tree => (app.tree.cursor, app.children().len()),
            Tab::TopFiles => (app.top_files, app.snapshot().top_files().len()),
            Tab::Extensions => (app.extensions, app.snapshot().ext_stats().len()),
            Tab::Info => (app.info, app.info_fields().len()),
            Tab::Search => (app.search, app.queries.search_items().len()),
        };
        assert_eq!(cursor.selected, 4.min(count.saturating_sub(1)));
    }
    Case::press(&mut app, KeyCode::Char('1'))?;
    Case::select(&mut app, case.id("/bigdir")?)?;
    app.handle(Event::Key(Key::Enter));
    app.handle(Event::Resize {
        width: 120,
        height: 30,
    });
    app.handle(Event::Key(Key::End));
    let last = app.tree.cursor.selected;
    app.handle(Event::Resize {
        width: 120,
        height: 8,
    });
    assert_eq!(
        app.tree.cursor.selected, last,
        "shrinking scrolls the selected item into view"
    );
    Case::assert_cursor(app.tree.cursor, app.children().len(), 4);
    assert!(!Case::selected_row(&app, 120, 8)?.trim().is_empty());
    app.handle(Event::Key(Key::Parent));
    assert_eq!(
        app.children().get(app.tree.cursor.selected),
        Some(&case.id("/bigdir")?)
    );
    Case::assert_cursor(app.tree.cursor, app.children().len(), 4);
    app.handle(Event::Resize {
        width: 120,
        height: 60,
    });
    assert_eq!(
        app.tree.cursor.scroll, 0,
        "growing clamps stale scroll offsets"
    );

    let case = Case::synthetic()?;
    let mut app = case.app("/", 30)?;
    Case::select(&mut app, case.id("/nested")?)?;
    let selected = app.tree.cursor.selected;
    assert!(
        selected >= 4,
        "ancestor selection must lie below the shrunken page"
    );
    assert_eq!(app.tree.cursor.scroll, 0);
    app.handle(Event::Key(Key::Enter));
    app.handle(Event::Key(Key::Enter));
    app.handle(Event::Resize {
        width: 120,
        height: 8,
    });
    app.handle(Event::Key(Key::Parent));
    app.handle(Event::Key(Key::Parent));
    assert_eq!(app.tree.directory, NodeId::ROOT);
    assert_eq!(app.tree.cursor.selected, selected);
    assert!(
        app.tree.cursor.scroll > 0,
        "saved ancestor viewport is resized while hidden"
    );
    assert!(Case::selected_row(&app, 120, 8)?.contains("nested/"));
    Case::assert_cursor(app.tree.cursor, app.children().len(), 4);
    Ok(())
}

#[test]
fn minimum_terminal_dimensions_replace_every_view_and_recover_after_resize() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/", 4)?;
    for code in ['1', '2', '3', '4'] {
        Case::press(&mut app, KeyCode::Char(code))?;
        for (width, height) in [(39, 8), (40, 7), (0, 0), (1, 1)] {
            app.handle(Event::Resize { width, height });
            let text = Case::text(&app, width, height)?;
            if width >= 39 {
                assert!(
                    text.contains("terminal too small"),
                    "{width}×{height}: {text}"
                );
            }
            assert!(
                !text.contains("Field") && !text.contains("Allocated"),
                "{text}"
            );
        }
        app.handle(Event::Resize {
            width: 40,
            height: 8,
        });
        let text = Case::text(&app, 40, 8)?;
        assert!(!text.contains("terminal too small"), "{text}");
        assert!(text.contains(app.tab.title()), "{text}");
    }
    Ok(())
}

fn terminal_key(app: &mut App<'_>, code: KeyCode) -> Action {
    app.handle_terminal(event::Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn input_text(app: &mut App<'_>, text: &str) {
    for character in text.chars() {
        assert_eq!(
            terminal_key(app, KeyCode::Char(character)),
            Action::Continue
        );
    }
}

fn confirm_text(app: &mut App<'_>, text: &str) {
    app.handle_terminal(event::Event::Key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    )));
    input_text(app, text);
    terminal_key(app, KeyCode::Enter);
}

#[test]
fn query_scope_refreshes_results_resets_cursors_and_displays_under_path() -> TestResult {
    use crate::queries::Groups;
    use std::borrow::Cow;
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/bigdir", 4)?;
    assert!(matches!(app.queries.top, Cow::Borrowed(_)));
    assert!(matches!(
        app.queries.groups,
        Groups::Extensions(Cow::Borrowed(_))
    ));
    terminal_key(&mut app, KeyCode::Char('/'));
    confirm_text(&mut app, "f*");
    app.handle(Event::Key(Key::End));
    terminal_key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.queries.scope, case.id("/bigdir")?);
    assert_eq!(app.search, Cursor::default());
    assert_eq!(
        app.queries.search.as_ref().ok_or("page")?.total_matches(),
        200
    );
    assert!(Case::text(&app, 120, 8)?.contains("under /bigdir"));
    terminal_key(&mut app, KeyCode::Char('2'));
    assert_eq!(app.queries.top.len(), 100);
    let bigdir = case.id("/bigdir")?;
    assert!(app.queries.top.iter().all(|id| {
        app.snapshot()
            .node(*id)
            .is_some_and(|node| node.parent() == bigdir)
    }));
    let top_ptr = app.queries.top.as_ptr();
    let search_ptr = app.queries.search_items().as_ptr();
    app.handle(Event::Key(Key::End));
    terminal_key(&mut app, KeyCode::Char('b'));
    let _ = Case::text(&app, 120, 8)?;
    terminal_key(&mut app, KeyCode::Char('3'));
    assert_eq!(app.queries.top.as_ptr(), top_ptr);
    assert_eq!(app.queries.search_items().as_ptr(), search_ptr);
    assert_eq!(app.queries.group_count(), 1);
    terminal_key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.queries.scope, NodeId::ROOT);
    for cursor in [app.top_files, app.extensions, app.search] {
        assert_eq!(cursor, Cursor::default());
    }
    assert!(matches!(app.queries.top, Cow::Borrowed(_)));
    assert!(!Case::text(&app, 120, 8)?.contains("under /bigdir"));
    Ok(())
}

#[test]
fn top_filter_input_commits_valid_filters_preserves_errors_and_clears() -> TestResult {
    use adamite_core::snapshot::{FileFilter, FileKind};
    use std::borrow::Cow;
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/", 4)?;
    terminal_key(&mut app, KeyCode::Char('2'));
    terminal_key(&mut app, KeyCode::Char('f'));
    assert!(Case::text(&app, 120, 8)?.contains("Filter:"));
    confirm_text(&mut app, ".mp4,mkv");
    assert_eq!(app.queries.filter, FileFilter::extensions(["mp4", "mkv"])?);
    assert!(app.queries.top.is_empty());
    assert!(Case::text(&app, 120, 8)?.contains("filter: mkv,mp4"));
    terminal_key(&mut app, KeyCode::Char('f'));
    confirm_text(&mut app, "kind:document");
    assert_eq!(app.queries.filter, FileFilter::Kind(FileKind::Document));
    let expected = app
        .snapshot()
        .rank_files(NodeId::ROOT, &app.queries.filter, 100)?;
    assert_eq!(app.queries.top.as_ref(), expected);
    assert!(Case::text(&app, 120, 8)?.contains("filter: kind:document"));
    let cached = app.queries.top.as_ptr();
    terminal_key(&mut app, KeyCode::Char('f'));
    confirm_text(&mut app, "kind:unknown");
    assert_eq!(app.queries.filter, FileFilter::Kind(FileKind::Document));
    assert_eq!(app.queries.top.as_ptr(), cached);
    assert!(Case::text(&app, 120, 8)?.contains("unknown file kind"));
    terminal_key(&mut app, KeyCode::Char('f'));
    confirm_text(&mut app, "");
    assert_eq!(app.queries.filter, FileFilter::All);
    assert!(matches!(app.queries.top, Cow::Borrowed(_)));
    assert!(app.status.is_none());
    assert!(!Case::text(&app, 120, 8)?.contains("filter:"));
    Ok(())
}

#[test]
fn extension_and_kind_rows_open_filtered_top_files_in_the_same_scope() -> TestResult {
    use crate::queries::Groups;
    use adamite_core::snapshot::{FileFilter, FileKind};
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/compressed", 4)?;
    terminal_key(&mut app, KeyCode::Char('u'));
    terminal_key(&mut app, KeyCode::Char('3'));
    assert!(Case::text(&app, 120, 8)?.contains("txt"));
    terminal_key(&mut app, KeyCode::Enter);
    assert_eq!(app.tab, Tab::TopFiles);
    assert_eq!(app.queries.filter, FileFilter::extensions(["txt"])?);
    assert_eq!(app.queries.top.as_ref(), [case.id("/compressed/c.txt")?]);
    terminal_key(&mut app, KeyCode::Enter);
    assert_eq!(app.tab, Tab::Tree);
    assert_eq!(app.tree.directory, case.id("/compressed")?);
    assert_eq!(
        app.children().get(app.tree.cursor.selected),
        Some(&case.id("/compressed/c.txt")?)
    );
    terminal_key(&mut app, KeyCode::Char('3'));
    terminal_key(&mut app, KeyCode::Char('g'));
    assert!(matches!(app.queries.groups, Groups::Kinds(_)));
    let text = Case::text(&app, 120, 8)?;
    for column in ["Kind", "Count", "Logical", "Allocated", "document"] {
        assert!(text.contains(column), "{text}");
    }
    terminal_key(&mut app, KeyCode::Enter);
    assert_eq!(app.tab, Tab::TopFiles);
    assert_eq!(app.queries.filter, FileFilter::Kind(FileKind::Document));
    assert_eq!(app.queries.scope, case.id("/compressed")?);
    terminal_key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.queries.scope, NodeId::ROOT);
    assert!(app.queries.top.len() > 1);
    assert!(matches!(&app.queries.groups, Groups::Kinds(stats) if stats.len() > 1));
    terminal_key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.queries.top.len(), 1);
    terminal_key(&mut app, KeyCode::Char('3'));
    terminal_key(&mut app, KeyCode::Char('g'));
    assert!(matches!(app.queries.groups, Groups::Extensions(_)));
    terminal_key(&mut app, KeyCode::Char('u'));
    app.handle(Event::Key(Key::Home));
    terminal_key(&mut app, KeyCode::Enter);
    assert_eq!(app.queries.filter, FileFilter::extensions([""])?);
    assert!(Case::text(&app, 120, 8)?.contains("filter: (none)"));
    Ok(())
}

#[test]
fn search_from_every_tab_handles_results_errors_and_directory_or_file_jumps() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/", 4)?;
    for tab in Tab::ALL {
        app.handle(Event::Key(Key::SelectTab(tab)));
        terminal_key(&mut app, KeyCode::Char('/'));
        assert_eq!(app.tab, Tab::Search);
        assert!(app.input.is_some());
        confirm_text(&mut app, "leaf");
        assert_eq!(
            app.queries.search_items(),
            [case.id("/nested/child/leaf.txt")?]
        );
        assert!(Case::text(&app, 120, 8)?.contains("/nested/child/leaf.txt"));
    }
    terminal_key(&mut app, KeyCode::Enter);
    assert_eq!(app.tab, Tab::Tree);
    assert_eq!(app.tree.directory, case.id("/nested/child")?);
    assert_eq!(
        app.children().get(app.tree.cursor.selected),
        Some(&case.id("/nested/child/leaf.txt")?)
    );
    terminal_key(&mut app, KeyCode::Char('/'));
    confirm_text(&mut app, "nested");
    assert!(Case::text(&app, 120, 8)?.contains("/nested/"));
    terminal_key(&mut app, KeyCode::Enter);
    assert_eq!(app.tab, Tab::Tree);
    assert_eq!(app.tree.directory, NodeId::ROOT);
    assert_eq!(
        app.children().get(app.tree.cursor.selected),
        Some(&case.id("/nested")?)
    );
    terminal_key(&mut app, KeyCode::Char('/'));
    confirm_text(&mut app, "not-present");
    assert!(app.queries.search_items().is_empty());
    assert!(Case::text(&app, 120, 8)?.contains("no matches"));
    terminal_key(&mut app, KeyCode::Char('/'));
    confirm_text(&mut app, "[");
    assert!(Case::text(&app, 120, 8)?.contains("invalid name glob"));
    assert_eq!(
        app.queries.pattern.as_ref().ok_or("pattern")?.as_str(),
        "not-present"
    );
    assert!(app.queries.search_items().is_empty());
    Ok(())
}

#[test]
fn search_tab_without_a_pattern_prompts_and_explains_itself() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/", 8)?;
    terminal_key(&mut app, KeyCode::Char('5'));
    assert_eq!(app.tab, Tab::Search);
    assert!(app.input.is_some());
    terminal_key(&mut app, KeyCode::Esc);
    assert!(app.input.is_none());
    let text = Case::text(&app, 120, 12)?;
    assert!(text.contains("Press / or Enter to search"), "{text}");
    terminal_key(&mut app, KeyCode::Enter);
    assert!(app.input.is_some());
    terminal_key(&mut app, KeyCode::Tab);
    assert!(app.input.is_none());
    assert_eq!(app.tab, Tab::Tree);
    terminal_key(&mut app, KeyCode::Char('5'));
    confirm_text(&mut app, "not-present");
    assert!(app.queries.search_items().is_empty());
    terminal_key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.input.as_ref().ok_or("input")?.editor.value(),
        "not-present"
    );
    terminal_key(&mut app, KeyCode::Esc);
    terminal_key(&mut app, KeyCode::Char('2'));
    terminal_key(&mut app, KeyCode::Char('5'));
    assert!(app.input.is_none(), "an existing pattern keeps its results");
    Ok(())
}

#[test]
fn input_routes_text_and_editing_keys_before_browser_shortcuts() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/", 4)?;
    terminal_key(&mut app, KeyCode::Char('2'));
    terminal_key(&mut app, KeyCode::Char('f'));
    input_text(&mut app, "q12");
    assert_eq!(app.tab, Tab::TopFiles);
    assert_eq!(app.input.as_ref().ok_or("input")?.editor.value(), "q12");
    for code in [
        KeyCode::Home,
        KeyCode::Right,
        KeyCode::Char('界'),
        KeyCode::Right,
        KeyCode::Backspace,
        KeyCode::End,
        KeyCode::Char('x'),
        KeyCode::Left,
        KeyCode::Backspace,
        KeyCode::Home,
        KeyCode::Backspace,
        KeyCode::End,
        KeyCode::Backspace,
    ] {
        assert_eq!(terminal_key(&mut app, code), Action::Continue);
    }
    assert_eq!(app.input.as_ref().ok_or("input")?.editor.value(), "q界");
    input_text(&mut app, "bf?/5");
    assert!(!app.raw_bytes && !app.help);
    assert_eq!(app.tab, Tab::TopFiles);
    let text = Case::text(&app, 80, 8)?;
    assert!(
        text.contains("Filter: q界") && text.contains("bf?/5"),
        "{text}"
    );
    assert_eq!(
        app.input.as_ref().ok_or("input")?.editor.value(),
        "q界bf?/5"
    );
    input_text(&mut app, &"a".repeat(100));
    let text = Case::text(&app, 40, 8)?;
    assert!(text.contains("Filter:") && text.contains("aaaa"), "{text}");
    assert_eq!(terminal_key(&mut app, KeyCode::Esc), Action::Continue);
    assert!(app.input.is_none());
    assert_eq!(app.queries.top.as_ref(), app.snapshot().top_files());
    terminal_key(&mut app, KeyCode::Char('/'));
    input_text(&mut app, "q");
    assert_eq!(app.tab, Tab::Search);
    assert_eq!(terminal_key(&mut app, KeyCode::Esc), Action::Continue);
    assert_eq!(terminal_key(&mut app, KeyCode::Char('q')), Action::Quit);
    Ok(())
}

#[test]
fn search_caps_cached_matches_reports_remaining_and_keeps_the_cursor_visible() -> TestResult {
    let case = Case::fixture("frag-512")?;
    let mut app = case.app("/growth", 4)?;
    terminal_key(&mut app, KeyCode::Char('u'));
    terminal_key(&mut app, KeyCode::Char('/'));
    confirm_text(&mut app, "*");
    let page = app.queries.search.as_ref().ok_or("page")?;
    assert_eq!(page.items().len(), 1000);
    assert_eq!(page.total_matches(), 1500);
    assert!(Case::text(&app, 120, 8)?.contains("500 more, refine the pattern"));
    let cached = page.items().as_ptr();
    app.handle(Event::Key(Key::End));
    assert!(!Case::selected_row(&app, 120, 8)?.trim().is_empty());
    terminal_key(&mut app, KeyCode::Char('/'));
    assert_eq!(app.queries.search_items().as_ptr(), cached);
    assert!(!Case::selected_row(&app, 120, 8)?.trim().is_empty());
    app.handle(Event::Resize {
        width: 120,
        height: 12,
    });
    assert!(!Case::selected_row(&app, 120, 12)?.trim().is_empty());
    terminal_key(&mut app, KeyCode::Esc);
    terminal_key(&mut app, KeyCode::Enter);
    assert_eq!(app.tab, Tab::Tree);
    assert_eq!(app.tree.directory, case.id("/growth")?);
    assert_eq!(
        app.children().get(app.tree.cursor.selected),
        Some(&case.id("/growth/f0999")?)
    );
    Ok(())
}

#[test]
fn query_help_search_bytes_and_input_repeat_events_render_correctly() -> TestResult {
    let case = Case::synthetic()?;
    let mut app = case.app("/", 4)?;
    terminal_key(&mut app, KeyCode::Char('?'));
    let help = Case::text(&app, 40, 8)?;
    for binding in [
        "Tab/1–5",
        "u: scope",
        "f: filter",
        "Ext g: kind",
        "/: search",
    ] {
        assert!(help.contains(binding), "{binding}: {help}");
    }
    terminal_key(&mut app, KeyCode::Esc);
    terminal_key(&mut app, KeyCode::Char('/'));
    input_text(&mut app, "q");
    for (kind, expected) in [(KeyEventKind::Release, "q"), (KeyEventKind::Repeat, "qx")] {
        app.handle_terminal(event::Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            kind,
        )));
        assert_eq!(app.input.as_ref().ok_or("input")?.editor.value(), expected);
    }
    confirm_text(&mut app, "leaf");
    terminal_key(&mut app, KeyCode::Char('b'));
    let row = Case::selected_row(&app, 120, 8)?;
    assert!(row.contains("/nested/child/leaf.txt"), "{row}");
    assert!(!row.contains(" B"), "{row}");
    let cached = app.queries.search_items().as_ptr();
    terminal_key(&mut app, KeyCode::Char('/'));
    confirm_text(&mut app, "leaf");
    assert_eq!(app.queries.search_items().as_ptr(), cached);
    terminal_key(&mut app, KeyCode::Char('/'));
    confirm_text(&mut app, &"x".repeat(257));
    assert!(Case::text(&app, 120, 8)?.contains("256-byte limit"));
    assert_eq!(
        app.queries.pattern.as_ref().ok_or("pattern")?.as_str(),
        "leaf"
    );
    Ok(())
}

#[test]
fn vim_up_remains_up_in_extensions_and_grouping_uses_g() -> TestResult {
    use crate::queries::Groups;
    let case = Case::fixture("basic-4k")?;
    let mut app = case.app("/", 4)?;
    terminal_key(&mut app, KeyCode::Char('3'));
    app.handle(Event::Key(Key::End));
    let selected = app.extensions.selected;
    assert!(selected > 0);
    terminal_key(&mut app, KeyCode::Char('k'));
    assert_eq!(app.extensions.selected, selected.saturating_sub(1));
    assert!(matches!(app.queries.groups, Groups::Extensions(_)));
    terminal_key(&mut app, KeyCode::Char('g'));
    assert!(matches!(app.queries.groups, Groups::Kinds(_)));
    app.handle(Event::Key(Key::End));
    let selected = app.extensions.selected;
    assert!(selected > 0);
    terminal_key(&mut app, KeyCode::Char('k'));
    assert_eq!(app.extensions.selected, selected.saturating_sub(1));
    assert!(matches!(app.queries.groups, Groups::Kinds(_)));
    terminal_key(&mut app, KeyCode::Char('2'));
    terminal_key(&mut app, KeyCode::Char('g'));
    assert!(matches!(app.queries.groups, Groups::Kinds(_)));
    terminal_key(&mut app, KeyCode::Char('f'));
    input_text(&mut app, "gk");
    assert_eq!(app.input.as_ref().ok_or("input")?.editor.value(), "gk");
    Ok(())
}

mod launcher;
