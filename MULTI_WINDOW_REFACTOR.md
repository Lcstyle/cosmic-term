# cosmic-term Multi-Window Refactoring Plan

## Goal
Enable true tab-to-window detachment by refactoring cosmic-term to use libcosmic's multi-window feature instead of spawning separate processes.

## Current Architecture (Problem)

```
┌─────────────────────────────────────────────────────────────┐
│ Process 1: cosmic-term                                       │
│   ┌─────────────────────────────────────────────────────┐   │
│   │ App                                                  │   │
│   │   pane_model: TerminalPaneGrid (single window)      │   │
│   │   terminal_ids: HashMap<Pane, Id>                   │   │
│   └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ Process 2: cosmic-term (SEPARATE PROCESS!)                   │
│   ┌─────────────────────────────────────────────────────┐   │
│   │ App (completely separate state)                      │   │
│   │   pane_model: TerminalPaneGrid                      │   │
│   │   terminal_ids: HashMap<Pane, Id>                   │   │
│   └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘

Problem: Terminals cannot be transferred between processes.
```

## Target Architecture (Solution)

```
┌─────────────────────────────────────────────────────────────┐
│ Process 1: cosmic-term (SINGLE PROCESS, MULTIPLE WINDOWS)    │
│   ┌─────────────────────────────────────────────────────┐   │
│   │ App                                                  │   │
│   │   windows: HashMap<window::Id, TerminalWindow>      │   │
│   │     ├─ Window A: TerminalPaneGrid + terminal_ids   │   │
│   │     └─ Window B: TerminalPaneGrid + terminal_ids   │   │
│   │   config, themes, etc. (shared)                     │   │
│   └─────────────────────────────────────────────────────┘   │
│                                                              │
│   Terminal entities can move between Window A ↔ Window B    │
└─────────────────────────────────────────────────────────────┘
```

---

## Implementation Phases

### Phase 1: Add Window Tracking Infrastructure

#### 1.1 Create WindowKind enum and Window struct

```rust
// In main.rs, after the existing enums

#[derive(Clone, Debug)]
pub enum WindowKind {
    /// Main terminal window with pane grid and tabs
    Terminal,
    /// File dialog windows
    Dialog,
}

struct TerminalWindow {
    kind: WindowKind,
    pane_model: TerminalPaneGrid,
    terminal_ids: HashMap<pane_grid::Pane, widget::Id>,
    modifiers: Modifiers,
}

impl TerminalWindow {
    fn new_terminal() -> Self {
        let pane_model = TerminalPaneGrid::new(
            segmented_button::ModelBuilder::default().build()
        );
        let mut terminal_ids = HashMap::new();
        terminal_ids.insert(pane_model.focused(), widget::Id::unique());

        Self {
            kind: WindowKind::Terminal,
            pane_model,
            terminal_ids,
            modifiers: Modifiers::empty(),
        }
    }
}
```

#### 1.2 Modify App struct

**Before:**
```rust
pub struct App {
    core: Core,
    pane_model: TerminalPaneGrid,      // Single window
    terminal_ids: HashMap<...>,         // Single window
    // ... other fields
}
```

**After:**
```rust
pub struct App {
    core: Core,
    windows: HashMap<window::Id, TerminalWindow>,  // All windows
    main_window_id: Option<window::Id>,            // Track main window
    // ... other shared fields (config, themes, etc.)
}
```

---

### Phase 2: Refactor Window Creation

#### 2.1 Change Message::WindowNew handler

**Before (spawns new process):**
```rust
Message::WindowNew => match env::current_exe() {
    Ok(exe) => match process::Command::new(&exe).spawn() {
        Ok(_child) => {}
        Err(err) => log::error!("failed to execute {:?}: {}", exe, err);
    },
    Err(err) => log::error!("failed to get current executable path: {}", err);
},
```

**After (creates window in same process):**
```rust
Message::WindowNew => {
    let settings = window::Settings {
        size: iced::Size::new(800.0, 600.0),
        min_size: Some(iced::Size::new(360.0, 180.0)),
        decorations: true,
        resizable: true,
        ..Default::default()
    };

    let (id, command) = window::open(settings);
    self.windows.insert(id, TerminalWindow::new_terminal());

    // Need to create first tab in new window
    return Task::batch([
        command.map(|_| cosmic::action::none()),
        self.create_terminal_in_window(id, self.get_default_profile()),
    ]);
}
```

#### 2.2 Add new messages for window management

```rust
pub enum Message {
    // ... existing messages ...

    // New window management messages
    WindowOpened(window::Id),
    WindowCloseRequested(window::Id),

    // Tab transfer messages
    TabDetachToNewWindow(segmented_button::Entity),
    TabMoveToWindow(segmented_button::Entity, window::Id),
}
```

---

### Phase 3: Expand view_window()

**Before (only dialogs):**
```rust
fn view_window(&self, window_id: window::Id) -> Element<'_, Message> {
    match &self.dialog_opt {
        Some(dialog) => dialog.view(window_id),
        None => widget::text("Unknown window ID").into(),
    }
}
```

**After (terminals + dialogs):**
```rust
fn view_window(&self, window_id: window::Id) -> Element<'_, Message> {
    // Check if this is a dialog window
    if let Some(dialog) = &self.dialog_opt {
        if dialog.is_window(window_id) {
            return dialog.view(window_id);
        }
    }

    // Check if this is a terminal window
    if let Some(term_window) = self.windows.get(&window_id) {
        match &term_window.kind {
            WindowKind::Terminal => {
                return self.render_terminal_window(window_id, term_window);
            }
            WindowKind::Dialog => {
                // Handle dialog-type windows
            }
        }
    }

    widget::text("Unknown window ID").into()
}
```

---

### Phase 4: Implement Tab Transfer

#### 4.1 TabDetachToNewWindow handler

```rust
Message::TabDetachToNewWindow(entity) => {
    // Find which window contains this entity
    let source_window_id = self.find_window_containing_entity(entity);

    if let Some(source_id) = source_window_id {
        // Create new window
        let settings = window::Settings { ... };
        let (new_window_id, command) = window::open(settings);

        // Create new TerminalWindow
        let mut new_window = TerminalWindow::new_terminal();

        // Transfer the terminal from source to new window
        if let Some(source_window) = self.windows.get_mut(&source_id) {
            if let Some(tab_model) = source_window.pane_model.active_mut() {
                // Get terminal data
                if let Some(terminal_mutex) = tab_model.data::<Mutex<Terminal>>(entity) {
                    // Remove from source
                    let tab_text = tab_model.text(entity).map(|s| s.to_string());
                    tab_model.remove(entity);

                    // Add to new window
                    let new_tab_model = new_window.pane_model.active_mut().unwrap();
                    let new_entity = new_tab_model
                        .insert()
                        .text(tab_text.unwrap_or_else(|| fl!("new-terminal")))
                        .closable()
                        .activate()
                        .id();

                    // Transfer the terminal data
                    new_tab_model.data_set::<Mutex<Terminal>>(new_entity, terminal_mutex.clone());
                }
            }
        }

        self.windows.insert(new_window_id, new_window);
        return command.map(|_| cosmic::action::none());
    }
}
```

**Key insight:** Since all windows are in the same process, we can directly move the `Mutex<Terminal>` between TabModels. The Terminal keeps its PTY connection, scroll history, and running processes intact.

#### 4.2 TabMoveToWindow handler (for drag-drop between windows)

```rust
Message::TabMoveToWindow(entity, target_window_id) => {
    let source_window_id = self.find_window_containing_entity(entity);

    if let Some(source_id) = source_window_id {
        if source_id != target_window_id {
            // Transfer terminal between windows
            // Similar logic to TabDetachToNewWindow but to existing window
        }
    }
}
```

---

### Phase 5: Update Event Routing

Messages need to include window context:

```rust
// In subscription()
event::listen_with(|event, _status, window_id| match event {
    Event::Keyboard(KeyEvent::KeyPressed { key, modifiers, .. }) => {
        Some(Message::Key(window_id, modifiers, key))
    }
    Event::Window(WindowEvent::CloseRequested) => {
        Some(Message::WindowCloseRequested(window_id))
    }
    // ...
})
```

Update handlers to use window context:
```rust
Message::TabNew => {
    // Now needs to know which window to create tab in
    let window_id = self.focused_window_id();
    self.create_terminal_in_window(window_id, self.get_default_profile())
}
```

---

## Helper Methods Needed

```rust
impl App {
    /// Find which window contains a given tab entity
    fn find_window_containing_entity(&self, entity: Entity) -> Option<window::Id> {
        for (window_id, window) in &self.windows {
            for (_pane, tab_model) in window.pane_model.panes.iter() {
                if tab_model.position(entity).is_some() {
                    return Some(*window_id);
                }
            }
        }
        None
    }

    /// Get the currently focused window
    fn focused_window_id(&self) -> window::Id {
        // Track focused window via WindowFocused messages
        self.main_window_id.unwrap_or(window::Id::MAIN)
    }

    /// Create a terminal in a specific window
    fn create_terminal_in_window(
        &mut self,
        window_id: window::Id,
        profile: Option<ProfileId>
    ) -> Task<Message> {
        if let Some(window) = self.windows.get_mut(&window_id) {
            // Similar to current create_and_focus_new_terminal
            // but operating on window.pane_model instead of self.pane_model
        }
        Task::none()
    }

    /// Render a terminal window
    fn render_terminal_window(
        &self,
        window_id: window::Id,
        window: &TerminalWindow,
    ) -> Element<'_, Message> {
        // Similar to current view() but using window.pane_model
    }
}
```

---

## Migration Strategy

1. **Phase 1:** Add infrastructure (WindowKind, TerminalWindow, windows HashMap)
   - Keep existing single-window behavior working
   - Main window stored in `windows` HashMap with known ID

2. **Phase 2:** Refactor window creation
   - Replace process::Command with window::open()
   - Test that new windows work

3. **Phase 3:** Expand view_window()
   - Render terminal content for secondary windows
   - Test multi-window display

4. **Phase 4:** Implement tab transfer
   - Add TabDetachToNewWindow
   - Test terminal state preservation

5. **Phase 5:** Polish and edge cases
   - Window close handling
   - Focus management
   - Menu updates for window context

---

## Files to Modify

| File | Changes |
|------|---------|
| `src/main.rs` | WindowKind, TerminalWindow, App struct, messages, handlers, view_window |
| `src/terminal.rs` | May need to update TerminalPaneGrid for per-window usage |
| `src/menu.rs` | Add window context to menu items |
| `src/key_bind.rs` | Add TabDetachToNewWindow keybind |
| `i18n/en/cosmic_term.ftl` | Add detach-tab string |

---

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| Terminal event routing breaks | Events include (pane, entity) - need to also include window_id |
| Focus management complexity | Track focused_window_id, update on WindowFocused |
| Config updates affect all windows | This is actually desired - shared config is a feature |
| Memory usage increases | Minimal - just HashMap overhead, terminals already exist |

---

## Success Criteria

1. ✅ Multiple terminal windows in single process
2. ✅ Tab can be detached to new window preserving terminal state
3. ✅ Running processes survive detachment
4. ✅ Scroll history preserved
5. ✅ Working directory preserved
6. ✅ No regression in single-window usage
