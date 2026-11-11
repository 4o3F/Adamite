use crate::{
    app::{Action, App},
    input::{Event, Key},
    view::View,
};
use adamite_core::{
    scan::{ScanConfig, ScanOutput},
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
    style::Modifier,
};
use std::{error::Error, path::Path};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Case {
    output: ScanOutput,
}

impl Case {
    fn fixture(name: &str) -> TestResult<Self> {
        let fixture = Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../tests/fixtures/ntfs/{name}.json")),
        )?;
        Self::scan(fixture.image())
    }

    fn scan(image: &[u8]) -> TestResult<Self> {
        let mut reader = SliceReader::new(image);
        let output = SyncDriver::new(ScanConfig::default())?.run(&mut reader)?;
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
            (23, 5, "long-漢字漢字漢字漢字漢字漢字-tail", true),
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
    assert!(text.contains("18.7 MiB"), "{text}");
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
    let long_path = "/long-漢字漢字漢字漢字漢字漢字-tail";
    Case::select(&mut app, case.id(long_path)?)?;
    let text = Case::lines(&Case::render(&app, 80, 8)?).join("\n");
    let row = text
        .lines()
        .find(|line| line.contains("long-"))
        .ok_or("long row")?;
    assert!(row.contains("…/"), "{row}");
    assert!(!row.contains("-tail"), "{row}");
    app.handle(Event::Key(Key::Enter));
    let text = Case::lines(&Case::render(&app, 30, 8)?).join("\n");
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
        (KeyCode::Esc, KeyModifiers::NONE, Key::Quit),
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
        (KeyCode::Right, KeyModifiers::NONE, Key::Enter),
        (KeyCode::Char('l'), KeyModifiers::NONE, Key::Enter),
        (KeyCode::Left, KeyModifiers::NONE, Key::Parent),
        (KeyCode::Backspace, KeyModifiers::NONE, Key::Parent),
        (KeyCode::Char('h'), KeyModifiers::NONE, Key::Parent),
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
