//! Tests for the TUI config panel: editing, toggling, navigation, and persistence.

#[cfg(test)]
mod tui_config_tests {
    use std::collections::HashMap;

    use crossterm::event::{KeyCode, KeyModifiers};

    use crate::commands::viz::VizOutput;
    use crate::tui::viz_viewer::state::{
        ConfigEditKind, ConfigSection, FocusedPanel, InputMode, RightPanelTab, VizApp,
    };

    // ── Helpers ──────────────────────────────────────────────────────────

    /// Build a minimal VizApp with a real temp dir for config persistence tests.
    fn make_config_test_app(workgraph_dir: &std::path::Path) -> VizApp {
        // Ensure the workgraph dir exists
        std::fs::create_dir_all(workgraph_dir).unwrap();
        // Create an empty graph.jsonl so load_viz doesn't fail
        std::fs::write(workgraph_dir.join("graph.jsonl"), "").unwrap();

        let viz = VizOutput {
            text: String::from("(empty graph)"),
            node_line_map: HashMap::new(),
            task_order: Vec::new(),
            forward_edges: HashMap::new(),
            reverse_edges: HashMap::new(),
            char_edge_map: HashMap::new(),
            cycle_members: HashMap::new(),
        };
        let mut app = VizApp::from_viz_output_for_test(&viz);
        app.workgraph_dir = workgraph_dir.to_path_buf();
        app.right_panel_visible = true;
        app.right_panel_tab = RightPanelTab::Config;
        app.focused_panel = FocusedPanel::RightPanel;
        app
    }

    /// Simulate a key press event through the event handler.
    fn press_key(app: &mut VizApp, code: KeyCode) {
        crate::tui::viz_viewer::event::handle_key(app, code, KeyModifiers::NONE);
    }

    /// Simulate a key press with modifiers.
    #[allow(dead_code)]
    fn press_key_mod(app: &mut VizApp, code: KeyCode, modifiers: KeyModifiers) {
        crate::tui::viz_viewer::event::handle_key(app, code, modifiers);
    }

    /// Type a string character by character.
    fn type_string(app: &mut VizApp, s: &str) {
        for c in s.chars() {
            press_key(app, KeyCode::Char(c));
        }
    }

    /// Find the index of a config entry by key.
    fn find_entry(app: &VizApp, key: &str) -> Option<usize> {
        app.config_panel
            .entries
            .iter()
            .position(|e| e.key == key)
    }

    /// Load config from disk for verification.
    fn load_config(workgraph_dir: &std::path::Path) -> workgraph::config::Config {
        workgraph::config::Config::load(workgraph_dir).unwrap()
    }

    // ── Toggle Tests ─────────────────────────────────────────────────────

    #[test]
    fn test_toggle_boolean_entry_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        // Write initial config with agency.auto_evaluate = false
        let mut config = workgraph::config::Config::default();
        config.agency.auto_evaluate = false;
        config.save(wg_dir).unwrap();

        app.load_config_panel();

        // Find the auto_evaluate entry
        let idx = find_entry(&app, "agency.auto_evaluate").expect("entry should exist");
        assert_eq!(app.config_panel.entries[idx].value, "off");

        // Select it and toggle
        app.config_panel.selected = idx;
        app.toggle_config_entry();

        // Entry should now be "on"
        assert_eq!(app.config_panel.entries[idx].value, "on");

        // Config on disk should reflect the change
        let disk_config = load_config(wg_dir);
        assert!(disk_config.agency.auto_evaluate);

        // Toggle back
        app.toggle_config_entry();
        assert_eq!(app.config_panel.entries[idx].value, "off");
        let disk_config = load_config(wg_dir);
        assert!(!disk_config.agency.auto_evaluate);
    }

    #[test]
    fn test_toggle_show_token_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.tui.show_token_counts = false;
        config.save(wg_dir).unwrap();

        app.load_config_panel();

        let idx = find_entry(&app, "tui.show_token_counts").expect("entry should exist");
        assert_eq!(app.config_panel.entries[idx].value, "off");

        app.config_panel.selected = idx;
        app.toggle_config_entry();

        assert_eq!(app.config_panel.entries[idx].value, "on");
        let disk_config = load_config(wg_dir);
        assert!(disk_config.tui.show_token_counts);
    }

    #[test]
    fn test_toggle_only_affects_toggle_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        // Select a TextInput entry
        let idx = find_entry(&app, "coordinator.max_agents").expect("entry should exist");
        app.config_panel.selected = idx;
        let original_value = app.config_panel.entries[idx].value.clone();

        // toggle_config_entry should be a no-op for non-Toggle entries
        app.toggle_config_entry();
        assert_eq!(app.config_panel.entries[idx].value, original_value);
    }

    #[test]
    fn test_toggle_via_space_key() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.agency.auto_assign = false;
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "agency.auto_assign").expect("entry should exist");
        app.config_panel.selected = idx;

        // Space should toggle in config tab
        press_key(&mut app, KeyCode::Char(' '));
        assert_eq!(app.config_panel.entries[idx].value, "on");
    }

    #[test]
    fn test_toggle_via_enter_key() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.agency.auto_triage = true;
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "agency.auto_triage").expect("entry should exist");
        app.config_panel.selected = idx;
        assert_eq!(app.config_panel.entries[idx].value, "on");

        // Enter on a Toggle should toggle it
        press_key(&mut app, KeyCode::Enter);
        assert_eq!(app.config_panel.entries[idx].value, "off");
    }

    // ── Text Editing Tests ───────────────────────────────────────────────

    #[test]
    fn test_text_edit_flow() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.coordinator.max_agents = 3;
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "coordinator.max_agents").expect("entry should exist");
        app.config_panel.selected = idx;
        assert_eq!(app.config_panel.entries[idx].value, "3");

        // Enter to start editing
        press_key(&mut app, KeyCode::Enter);
        assert!(app.config_panel.editing);
        assert_eq!(app.input_mode, InputMode::ConfigEdit);
        assert_eq!(app.config_panel.edit_buffer, "3");

        // Clear and type new value
        press_key(&mut app, KeyCode::Backspace);
        type_string(&mut app, "5");
        assert_eq!(app.config_panel.edit_buffer, "5");

        // Enter to confirm
        press_key(&mut app, KeyCode::Enter);
        assert!(!app.config_panel.editing);
        assert_eq!(app.config_panel.entries[idx].value, "5");

        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.coordinator.max_agents, 5);
    }

    #[test]
    fn test_text_edit_cancel_with_esc() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.coordinator.max_agents = 3;
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "coordinator.max_agents").expect("entry should exist");
        app.config_panel.selected = idx;

        // Enter to start editing
        press_key(&mut app, KeyCode::Enter);
        assert!(app.config_panel.editing);

        // Type a new value
        press_key(&mut app, KeyCode::Backspace);
        type_string(&mut app, "99");

        // Esc to cancel
        press_key(&mut app, KeyCode::Esc);
        assert!(!app.config_panel.editing);
        assert_eq!(app.input_mode, InputMode::Normal);

        // Value should not have changed
        assert_eq!(app.config_panel.entries[idx].value, "3");
        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.coordinator.max_agents, 3);
    }

    #[test]
    fn test_text_edit_empty_string() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.coordinator.agent_timeout = "30m".to_string();
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "coordinator.agent_timeout").expect("entry should exist");
        app.config_panel.selected = idx;

        // Enter, clear entirely, confirm
        press_key(&mut app, KeyCode::Enter);
        // Delete all characters
        for _ in 0..10 {
            press_key(&mut app, KeyCode::Backspace);
        }
        press_key(&mut app, KeyCode::Enter);

        // Value should be empty string
        assert_eq!(app.config_panel.entries[idx].value, "");
        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.coordinator.agent_timeout, "");
    }

    #[test]
    fn test_text_edit_special_characters() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "coordinator.agent_timeout").expect("entry should exist");
        app.config_panel.selected = idx;

        // Enter to start editing, clear, type special chars
        press_key(&mut app, KeyCode::Enter);
        for _ in 0..20 {
            press_key(&mut app, KeyCode::Backspace);
        }
        type_string(&mut app, "1h30m");
        press_key(&mut app, KeyCode::Enter);

        assert_eq!(app.config_panel.entries[idx].value, "1h30m");
        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.coordinator.agent_timeout, "1h30m");
    }

    // ── Choice Editing Tests ─────────────────────────────────────────────

    #[test]
    fn test_choice_edit_flow() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.coordinator.executor = "claude".to_string();
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "coordinator.executor").expect("entry should exist");
        app.config_panel.selected = idx;
        assert_eq!(app.config_panel.entries[idx].value, "claude");

        // Enter to start editing
        press_key(&mut app, KeyCode::Enter);
        assert!(app.config_panel.editing);
        assert_eq!(app.config_panel.choice_index, 0); // "claude" is first

        // Right arrow to next choice ("amplifier")
        press_key(&mut app, KeyCode::Right);
        assert_eq!(app.config_panel.choice_index, 1);

        // Enter to confirm
        press_key(&mut app, KeyCode::Enter);
        assert!(!app.config_panel.editing);
        assert_eq!(app.config_panel.entries[idx].value, "amplifier");

        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.coordinator.executor, "amplifier");
    }

    #[test]
    fn test_choice_edit_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.coordinator.executor = "claude".to_string();
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "coordinator.executor").expect("entry should exist");
        app.config_panel.selected = idx;

        press_key(&mut app, KeyCode::Enter);
        press_key(&mut app, KeyCode::Right); // Move to "amplifier"
        press_key(&mut app, KeyCode::Esc); // Cancel

        assert!(!app.config_panel.editing);
        assert_eq!(app.config_panel.entries[idx].value, "claude");
    }

    #[test]
    fn test_choice_left_right_bounds() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "coordinator.executor").expect("entry should exist");
        app.config_panel.selected = idx;

        // Enter to edit
        press_key(&mut app, KeyCode::Enter);

        // At index 0, Left should not go below 0
        press_key(&mut app, KeyCode::Left);
        assert_eq!(app.config_panel.choice_index, 0);

        // Move right several times to reach the end
        let num_choices = match &app.config_panel.entries[idx].edit_kind {
            ConfigEditKind::Choice(c) => c.len(),
            _ => panic!("expected Choice"),
        };
        for _ in 0..num_choices + 5 {
            press_key(&mut app, KeyCode::Right);
        }
        assert_eq!(app.config_panel.choice_index, num_choices - 1);

        press_key(&mut app, KeyCode::Esc);
    }

    // ── Section Collapse Tests ───────────────────────────────────────────

    #[test]
    fn test_section_collapse_toggle() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);
        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        // Initially no sections collapsed
        assert!(app.config_panel.collapsed.is_empty());
        let initial_visible = app.visible_config_entries().len();
        assert!(initial_visible > 0);

        // Collapse the Agency section
        app.toggle_config_section(ConfigSection::Agency);
        assert!(app.config_panel.collapsed.contains(&ConfigSection::Agency));

        let after_collapse = app.visible_config_entries().len();
        assert!(after_collapse < initial_visible);

        // Expand it back
        app.toggle_config_section(ConfigSection::Agency);
        assert!(!app.config_panel.collapsed.contains(&ConfigSection::Agency));
        assert_eq!(app.visible_config_entries().len(), initial_visible);
    }

    #[test]
    fn test_collapsed_entries_filtered_from_visible() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);
        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        // Count Agency entries
        let agency_count = app
            .config_panel
            .entries
            .iter()
            .filter(|e| e.section == ConfigSection::Agency)
            .count();
        assert!(agency_count > 0);

        let before = app.visible_config_entries().len();

        app.toggle_config_section(ConfigSection::Agency);

        let after = app.visible_config_entries().len();
        assert_eq!(after, before - agency_count);

        // Verify no Agency entries appear in visible list
        for (_, entry) in app.visible_config_entries() {
            assert_ne!(entry.section, ConfigSection::Agency);
        }
    }

    // ── Navigation Tests ─────────────────────────────────────────────────

    #[test]
    fn test_navigation_down() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);
        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        // Start at first visible entry
        let visible = app.visible_config_entries();
        app.config_panel.selected = visible[0].0;
        let first = app.config_panel.selected;

        // Navigate down
        press_key(&mut app, KeyCode::Char('j'));
        assert!(app.config_panel.selected > first);
    }

    #[test]
    fn test_navigation_up() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);
        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        // Navigate to second entry first
        let visible = app.visible_config_entries();
        if visible.len() < 2 {
            return; // skip test if not enough entries
        }
        app.config_panel.selected = visible[1].0;
        let second = app.config_panel.selected;

        press_key(&mut app, KeyCode::Char('k'));
        assert!(app.config_panel.selected < second);
    }

    #[test]
    fn test_navigation_home_end() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);
        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        let visible = app.visible_config_entries();
        let first = visible.first().unwrap().0;
        let last = visible.last().unwrap().0;

        // Go to end
        press_key(&mut app, KeyCode::End);
        assert_eq!(app.config_panel.selected, last);

        // Go to home
        press_key(&mut app, KeyCode::Home);
        assert_eq!(app.config_panel.selected, first);
    }

    #[test]
    fn test_navigation_skips_collapsed_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);
        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        // Collapse Service section
        app.toggle_config_section(ConfigSection::Service);

        // Find the last Endpoints entry and select it
        let last_endpoints_idx = app
            .config_panel
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.section == ConfigSection::Endpoints)
            .last()
            .map(|(i, _)| i)
            .unwrap();

        app.config_panel.selected = last_endpoints_idx;

        // Navigate down - should skip all Service entries and land in TuiSettings (or next visible section)
        press_key(&mut app, KeyCode::Char('j'));

        let selected_section = app.config_panel.entries[app.config_panel.selected].section;
        assert_ne!(
            selected_section,
            ConfigSection::Service,
            "Navigation should skip collapsed Service section"
        );
    }

    // ── Config Persistence Tests ─────────────────────────────────────────

    #[test]
    fn test_config_file_not_corrupted_after_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        // Start with a rich config
        let mut config = workgraph::config::Config::default();
        config.coordinator.max_agents = 3;
        config.coordinator.executor = "claude".to_string();
        config.agency.auto_evaluate = true;
        config.tui.show_token_counts = true;
        config.guardrails.max_child_tasks_per_agent = 25;
        config.save(wg_dir).unwrap();

        app.load_config_panel();

        // Edit max_agents
        let idx = find_entry(&app, "coordinator.max_agents").unwrap();
        app.config_panel.selected = idx;
        press_key(&mut app, KeyCode::Enter);
        press_key(&mut app, KeyCode::Backspace);
        type_string(&mut app, "7");
        press_key(&mut app, KeyCode::Enter);

        // Verify ALL config fields survived
        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.coordinator.max_agents, 7); // changed
        assert_eq!(disk_config.coordinator.executor, "claude"); // unchanged
        assert!(disk_config.agency.auto_evaluate); // unchanged
        assert!(disk_config.tui.show_token_counts); // unchanged
        assert_eq!(disk_config.guardrails.max_child_tasks_per_agent, 25); // unchanged
    }

    #[test]
    fn test_config_toml_is_valid_after_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();

        app.load_config_panel();

        // Toggle a boolean
        let idx = find_entry(&app, "agency.auto_evaluate").unwrap();
        app.config_panel.selected = idx;
        app.toggle_config_entry();

        // Read the file and ensure it's valid TOML
        let content = std::fs::read_to_string(wg_dir.join("config.toml")).unwrap();
        let parsed: Result<toml::Value, _> = content.parse();
        assert!(parsed.is_ok(), "Config file should be valid TOML");

        // Also ensure it can be deserialized back to Config
        let deserialized: Result<workgraph::config::Config, _> = toml::from_str(&content);
        assert!(
            deserialized.is_ok(),
            "Config file should deserialize to Config struct"
        );
    }

    // ── Reload Tests ─────────────────────────────────────────────────────

    #[test]
    fn test_reload_config_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.coordinator.max_agents = 3;
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "coordinator.max_agents").unwrap();
        assert_eq!(app.config_panel.entries[idx].value, "3");

        // Modify config externally
        config.coordinator.max_agents = 10;
        config.save(wg_dir).unwrap();

        // Reload (r key)
        press_key(&mut app, KeyCode::Char('r'));

        let idx = find_entry(&app, "coordinator.max_agents").unwrap();
        assert_eq!(app.config_panel.entries[idx].value, "10");
    }

    #[test]
    fn test_reload_preserves_selection_index() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();

        app.load_config_panel();

        // Select some entry (not the first)
        app.config_panel.selected = 5;

        // Reload
        press_key(&mut app, KeyCode::Char('r'));

        // Index should be preserved (or clamped if entries changed)
        assert_eq!(app.config_panel.selected, 5);
    }

    // ── Edge Cases ───────────────────────────────────────────────────────

    #[test]
    fn test_load_config_panel_no_config_file() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        std::fs::create_dir_all(wg_dir).unwrap();
        std::fs::write(wg_dir.join("graph.jsonl"), "").unwrap();

        let mut app = make_config_test_app(wg_dir);
        // No config.toml exists — should still load entries (from defaults/global)
        app.load_config_panel();

        assert!(!app.config_panel.entries.is_empty());

        // Should have a coordinator.max_agents entry with a parseable numeric value
        let idx = find_entry(&app, "coordinator.max_agents").unwrap();
        let val: usize = app.config_panel.entries[idx]
            .value
            .parse()
            .expect("max_agents should be a valid number");
        assert!(val > 0, "max_agents default should be positive");
    }

    #[test]
    fn test_config_with_unknown_fields_forward_compat() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        std::fs::create_dir_all(wg_dir).unwrap();
        std::fs::write(wg_dir.join("graph.jsonl"), "").unwrap();

        // Write a config with unknown fields
        let content = r#"
[coordinator]
max_agents = 5

[some_future_section]
new_setting = "hello"
"#;
        std::fs::write(wg_dir.join("config.toml"), content).unwrap();

        let mut app = make_config_test_app(wg_dir);
        app.load_config_panel();

        // Should have loaded max_agents correctly despite unknown section
        let idx = find_entry(&app, "coordinator.max_agents").unwrap();
        assert_eq!(app.config_panel.entries[idx].value, "5");
    }

    #[test]
    fn test_save_notification_set_on_toggle() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        assert!(app.config_panel.save_notification.is_none());

        let idx = find_entry(&app, "agency.auto_evaluate").unwrap();
        app.config_panel.selected = idx;
        app.toggle_config_entry();

        assert!(app.config_panel.save_notification.is_some());
    }

    #[test]
    fn test_save_notification_set_on_text_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        let idx = find_entry(&app, "coordinator.max_agents").unwrap();
        app.config_panel.selected = idx;

        // Edit and save
        press_key(&mut app, KeyCode::Enter);
        press_key(&mut app, KeyCode::Backspace);
        type_string(&mut app, "9");
        press_key(&mut app, KeyCode::Enter);

        assert!(app.config_panel.save_notification.is_some());
    }

    // ── Endpoint Management Tests ────────────────────────────────────────

    #[test]
    fn test_add_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        // Verify no endpoints initially
        let disk_config = load_config(wg_dir);
        assert!(disk_config.llm_endpoints.endpoints.is_empty());

        // Use add_endpoint directly (simulating form completion)
        app.config_panel.new_endpoint.name = "test-ep".to_string();
        app.config_panel.new_endpoint.provider = "anthropic".to_string();
        app.config_panel.new_endpoint.url = "https://api.example.com".to_string();
        app.config_panel.new_endpoint.model = "claude-3-opus".to_string();
        app.config_panel.new_endpoint.api_key = "sk-test-123".to_string();
        app.add_endpoint();

        // Verify on disk
        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.llm_endpoints.endpoints.len(), 1);
        assert_eq!(disk_config.llm_endpoints.endpoints[0].name, "test-ep");
        assert_eq!(disk_config.llm_endpoints.endpoints[0].provider, "anthropic");
        assert!(disk_config.llm_endpoints.endpoints[0].is_default); // first endpoint is default
    }

    #[test]
    fn test_add_endpoint_requires_name() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        // Try to add with empty name
        app.config_panel.new_endpoint.name = "".to_string();
        app.add_endpoint();

        // Should not have been added
        let disk_config = load_config(wg_dir);
        assert!(disk_config.llm_endpoints.endpoints.is_empty());
    }

    #[test]
    fn test_remove_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        // Add an endpoint first
        let mut config = workgraph::config::Config::default();
        config
            .llm_endpoints
            .endpoints
            .push(workgraph::config::EndpointConfig {
                name: "to-remove".to_string(),
                provider: "anthropic".to_string(),
                url: None,
                model: None,
                api_key: None,
                is_default: true,
            });
        config.save(wg_dir).unwrap();

        app.load_config_panel();

        // Find the remove entry
        let idx = find_entry(&app, "endpoint.0.remove").expect("remove entry should exist");
        app.config_panel.selected = idx;

        // Toggle the remove entry (triggers removal)
        app.toggle_config_entry();

        let disk_config = load_config(wg_dir);
        assert!(disk_config.llm_endpoints.endpoints.is_empty());
    }

    #[test]
    fn test_set_default_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config
            .llm_endpoints
            .endpoints
            .push(workgraph::config::EndpointConfig {
                name: "ep1".to_string(),
                provider: "anthropic".to_string(),
                url: None,
                model: None,
                api_key: None,
                is_default: true,
            });
        config
            .llm_endpoints
            .endpoints
            .push(workgraph::config::EndpointConfig {
                name: "ep2".to_string(),
                provider: "openai".to_string(),
                url: None,
                model: None,
                api_key: None,
                is_default: false,
            });
        config.save(wg_dir).unwrap();

        app.load_config_panel();

        // Set ep2 as default
        let idx = find_entry(&app, "endpoint.1.is_default").expect("is_default entry should exist");
        app.config_panel.selected = idx;
        app.toggle_config_entry();

        let disk_config = load_config(wg_dir);
        assert!(
            !disk_config.llm_endpoints.endpoints[0].is_default,
            "ep1 should no longer be default"
        );
        assert!(
            disk_config.llm_endpoints.endpoints[1].is_default,
            "ep2 should now be default"
        );
    }

    // ── Numeric Parse Edge Cases ─────────────────────────────────────────

    #[test]
    fn test_invalid_numeric_input_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.coordinator.max_agents = 3;
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "coordinator.max_agents").unwrap();
        app.config_panel.selected = idx;

        // Enter, clear, type non-numeric value
        press_key(&mut app, KeyCode::Enter);
        for _ in 0..5 {
            press_key(&mut app, KeyCode::Backspace);
        }
        type_string(&mut app, "abc");
        press_key(&mut app, KeyCode::Enter);

        // The entry value gets set to "abc" but the numeric field should be unchanged
        let disk_config = load_config(wg_dir);
        assert_eq!(
            disk_config.coordinator.max_agents, 3,
            "Non-numeric input should not change numeric config field"
        );
    }

    // ── Add Endpoint Flow via Key Events ─────────────────────────────────

    #[test]
    fn test_add_endpoint_flow_via_a_key() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        // Press 'a' to start add-endpoint flow
        press_key(&mut app, KeyCode::Char('a'));
        assert!(app.config_panel.adding_endpoint);
        assert_eq!(app.input_mode, InputMode::ConfigEdit);
        assert_eq!(app.config_panel.new_endpoint_field, 0);
    }

    #[test]
    fn test_add_endpoint_esc_cancels() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        press_key(&mut app, KeyCode::Char('a'));
        assert!(app.config_panel.adding_endpoint);

        press_key(&mut app, KeyCode::Esc);
        assert!(!app.config_panel.adding_endpoint);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    // ── Multiple Toggles in Sequence ─────────────────────────────────────

    #[test]
    fn test_multiple_toggles_consistent() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.agency.auto_assign = false;
        config.agency.auto_evaluate = false;
        config.agency.auto_triage = false;
        config.save(wg_dir).unwrap();

        app.load_config_panel();

        // Toggle auto_assign on
        let idx1 = find_entry(&app, "agency.auto_assign").unwrap();
        app.config_panel.selected = idx1;
        app.toggle_config_entry();

        // Toggle auto_evaluate on
        let idx2 = find_entry(&app, "agency.auto_evaluate").unwrap();
        app.config_panel.selected = idx2;
        app.toggle_config_entry();

        // Verify both changes persisted (not overwritten by each other)
        let disk_config = load_config(wg_dir);
        assert!(disk_config.agency.auto_assign);
        assert!(disk_config.agency.auto_evaluate);
        assert!(!disk_config.agency.auto_triage); // was not toggled
    }

    // ── Animation Speed Choice ───────────────────────────────────────────

    #[test]
    fn test_animation_speed_choice_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.viz.animations = "normal".to_string();
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "viz.animations").unwrap();
        app.config_panel.selected = idx;

        // Enter to start choice editing
        press_key(&mut app, KeyCode::Enter);
        assert!(app.config_panel.editing);

        // Navigate to "fast" (index 1)
        press_key(&mut app, KeyCode::Right);
        press_key(&mut app, KeyCode::Enter);

        assert_eq!(app.config_panel.entries[idx].value, "fast");
        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.viz.animations, "fast");
    }

    // ── Guardrails Config ────────────────────────────────────────────────

    #[test]
    fn test_guardrails_max_subtasks_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.guardrails.max_child_tasks_per_agent = 25;
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "guardrails.max_child_tasks_per_agent").unwrap();
        app.config_panel.selected = idx;

        press_key(&mut app, KeyCode::Enter);
        // Clear "25"
        press_key(&mut app, KeyCode::Backspace);
        press_key(&mut app, KeyCode::Backspace);
        type_string(&mut app, "50");
        press_key(&mut app, KeyCode::Enter);

        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.guardrails.max_child_tasks_per_agent, 50);
    }

    #[test]
    fn test_guardrails_max_depth_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let mut config = workgraph::config::Config::default();
        config.guardrails.max_task_depth = 8;
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "guardrails.max_task_depth").unwrap();
        app.config_panel.selected = idx;

        press_key(&mut app, KeyCode::Enter);
        press_key(&mut app, KeyCode::Backspace);
        type_string(&mut app, "12");
        press_key(&mut app, KeyCode::Enter);

        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.guardrails.max_task_depth, 12);
    }

    // ── Mouse Mode Choice ────────────────────────────────────────────────

    #[test]
    fn test_mouse_mode_choice() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "tui.mouse_mode").unwrap();
        app.config_panel.selected = idx;

        // Default is "auto"
        press_key(&mut app, KeyCode::Enter);
        // Navigate to "on" (index 1)
        press_key(&mut app, KeyCode::Right);
        press_key(&mut app, KeyCode::Enter);

        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.tui.mouse_mode, Some(true));
    }

    // ── Config Entry Types Coverage ──────────────────────────────────────

    #[test]
    fn test_all_edit_kinds_present() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);
        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        let has_toggle = app
            .config_panel
            .entries
            .iter()
            .any(|e| matches!(e.edit_kind, ConfigEditKind::Toggle));
        let has_choice = app
            .config_panel
            .entries
            .iter()
            .any(|e| matches!(e.edit_kind, ConfigEditKind::Choice(_)));
        let has_text = app
            .config_panel
            .entries
            .iter()
            .any(|e| matches!(e.edit_kind, ConfigEditKind::TextInput));
        let has_secret = app
            .config_panel
            .entries
            .iter()
            .any(|e| matches!(e.edit_kind, ConfigEditKind::SecretInput));

        assert!(has_toggle, "Should have Toggle entries");
        assert!(has_choice, "Should have Choice entries");
        assert!(has_text, "Should have TextInput entries");
        assert!(has_secret, "Should have SecretInput entries");
    }

    #[test]
    fn test_all_sections_present() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);
        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();
        app.load_config_panel();

        for section in ConfigSection::all() {
            let has_section = app
                .config_panel
                .entries
                .iter()
                .any(|e| e.section == *section);
            assert!(
                has_section,
                "Should have entries for section {:?}",
                section
            );
        }
    }

    // ── Message Indent Clamping ──────────────────────────────────────────

    #[test]
    fn test_message_indent_clamped_to_8() {
        let tmp = tempfile::tempdir().unwrap();
        let wg_dir = tmp.path();
        let mut app = make_config_test_app(wg_dir);

        let config = workgraph::config::Config::default();
        config.save(wg_dir).unwrap();

        app.load_config_panel();
        let idx = find_entry(&app, "tui.message_indent").unwrap();
        app.config_panel.selected = idx;

        press_key(&mut app, KeyCode::Enter);
        // Clear and type a value > 8
        for _ in 0..5 {
            press_key(&mut app, KeyCode::Backspace);
        }
        type_string(&mut app, "20");
        press_key(&mut app, KeyCode::Enter);

        let disk_config = load_config(wg_dir);
        assert_eq!(disk_config.tui.message_indent, 8, "Should be clamped to 8");
        assert_eq!(app.message_indent, 8);
    }
}
