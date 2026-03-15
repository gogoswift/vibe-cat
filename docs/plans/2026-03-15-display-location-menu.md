# Display Location Menu Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a runtime-only multi-display "Display Location" tray submenu so the cat can stay in Auto mode or be pinned to a user-selected display.

**Architecture:** Reuse the existing Dock sampling and `DockPlacementSample -> AppliedLayout` pipeline, and only add a display-location preference layer in front of it. The tray owns runtime-only selection state and dynamically rebuilds the display submenu from live screen snapshots; the cat layout code interprets that state and either follows Dock automatically or forces a floor-style layout on the chosen display.

**Tech Stack:** Rust, eframe/egui, objc2/objc2-app-kit, macOS NSScreen sampling, existing tray + cat layout modules.

---

### Task 1: Write down the display-location state model

**Files:**
- Modify: `src/tray.rs`
- Modify: `src/cat.rs`
- Test: `src/cat.rs`

**Step 1: Write the failing test**

Add tests that describe:

```rust
#[test]
fn manual_display_mode_should_fall_back_to_auto_when_selection_disappears() {
    // selected display missing -> mode becomes auto
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test manual_display_mode -- --nocapture`
Expected: FAIL because the display-location state and resolver do not exist yet.

**Step 3: Write minimal implementation**

Add a small runtime state model for:

```rust
enum DisplayLocationMode {
    Auto,
    Specific(String),
}
```

Also add the minimum helper needed to resolve missing selections back to `Auto`.

**Step 4: Run test to verify it passes**

Run: `cargo test manual_display_mode -- --nocapture`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/tray.rs src/cat.rs
git commit -m "实现显示位置运行时状态"
```

### Task 2: Add layout tests for manual display selection

**Files:**
- Modify: `src/cat.rs`
- Test: `src/cat.rs`

**Step 1: Write the failing test**

Add tests for:

```rust
#[test]
fn manual_display_mode_should_use_floor_layout_when_dock_is_on_other_display() {
    // selected display != dock display -> floor mode on selected display
}

#[test]
fn manual_display_mode_should_reuse_dock_layout_when_dock_is_on_selected_display() {
    // selected display == dock display -> keep dock-attached layout
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test manual_display_mode_should -- --nocapture`
Expected: FAIL because manual mode is not wired into layout resolution.

**Step 3: Write minimal implementation**

Extend the Dock sample resolution path so it can:

- honor `Auto`
- find the selected display in live snapshots
- reuse bottom/floor dock sample when Dock is on that display
- synthesize a floor-mode sample when Dock is elsewhere

**Step 4: Run test to verify it passes**

Run: `cargo test manual_display_mode_should -- --nocapture`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/cat.rs
git commit -m "实现手动显示器布局分支"
```

### Task 3: Expose live display choices for the tray

**Files:**
- Modify: `src/cat.rs`
- Modify: `src/tray.rs`
- Test: `src/cat.rs`

**Step 1: Write the failing test**

Add a test for duplicate naming / menu choice shaping, for example:

```rust
#[test]
fn display_menu_choices_should_suffix_duplicate_names() {
    // two displays with same name -> distinct labels
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test display_menu_choices -- --nocapture`
Expected: FAIL because the tray-facing display choice snapshot helper does not exist.

**Step 3: Write minimal implementation**

Add a tray-facing display snapshot helper that returns:

- runtime selection id
- user-facing label
- whether the display is main
- current display count

Keep AppKit reads on the main thread.

**Step 4: Run test to verify it passes**

Run: `cargo test display_menu_choices -- --nocapture`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/cat.rs src/tray.rs
git commit -m "提供托盘显示器菜单快照"
```

### Task 4: Build the dynamic tray submenu

**Files:**
- Modify: `src/tray.rs`
- Modify: `src/i18n.rs`

**Step 1: Write the failing test**

Where direct Cocoa menu tests are hard, add small pure helpers that can be tested separately, such as:

```rust
#[test]
fn display_location_menu_visibility_should_stay_enabled_after_multidisplay_seen() {
    // once visible in this session, stay visible
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test display_location_menu_visibility -- --nocapture`
Expected: FAIL because the visibility policy helper does not exist.

**Step 3: Write minimal implementation**

- Add i18n keys for display-location menu copy.
- Add a runtime flag like “has seen multiple displays this session”.
- Rebuild or refresh the submenu before popup.
- Add menu handlers for `Automatic` and per-display selection.
- Show the parent submenu only when the session policy says it should be visible.
- When only one display remains after multi-display was seen, keep the submenu and show a disabled informational row.

**Step 4: Run test to verify it passes**

Run: `cargo test display_location_menu_visibility -- --nocapture`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/tray.rs src/i18n.rs
git commit -m "实现托盘显示位置动态菜单"
```

### Task 5: Verify the whole feature end-to-end

**Files:**
- Modify: `src/cat.rs`
- Modify: `src/tray.rs`
- Modify: `src/i18n.rs`

**Step 1: Run focused tests**

Run: `cargo test manual_display_mode -- --nocapture`
Expected: PASS.

Run: `cargo test display_menu_choices -- --nocapture`
Expected: PASS.

Run: `cargo test display_location_menu_visibility -- --nocapture`
Expected: PASS.

**Step 2: Run full test suite**

Run: `cargo test -- --nocapture`
Expected: all tests PASS.

**Step 3: Manual verification**

- Start app with one display: no “显示位置” submenu.
- Connect second display: right-click tray, submenu appears.
- Leave mode on “自动”: cat follows Dock display.
- Pick a non-Dock display: cat moves to that display bottom edge.
- Move Dock onto the chosen display: cat snaps to Dock top edge.
- Unplug the chosen display: mode falls back to “自动”.
- If only one display remains, submenu stays visible for this run and shows the single-display info row.

**Step 4: Final commit**

```bash
git add src/cat.rs src/tray.rs src/i18n.rs
git commit -m "支持托盘切换像素猫显示位置"
```
