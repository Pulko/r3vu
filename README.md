# r3vu

A mouse-driven terminal IDE for TypeScript and JavaScript on macOS.

```bash
cargo install r3vu
r3vu
```

The crates.io package is `r3vu`. The command it installs is `r3vu`.

That opens the current directory. Pass a path to open another project. Hyper is the terminal it is built for.

Click to place the cursor. Type to insert. There are no modes.

## Shortcuts

| Action | Shortcut |
| --- | --- |
| Save | Ctrl+S |
| Open | Ctrl+P |
| Search in file | Ctrl+F |
| Go to definition | Ctrl+Enter or Ctrl+Click |
| Ask Claude | Option+T |
| Edit with Claude | Option+E |
| Rename symbol | Ctrl+R |
| Undo / redo | Ctrl+Z / Ctrl+Y |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Git diff | Ctrl+Shift+D |
| Git panel | Ctrl+Shift+G |
| Preview Markdown | Ctrl+Shift+M |
| Toggle explorer | Ctrl+B |
| Close file | Ctrl+W |
| New file | Ctrl+N |
| Quit | Ctrl+Q |
| Shortcuts | Ctrl+/ |
| Close a popup | Esc |

Click **Open** above the explorer, or press Ctrl+P, to open a file by name and then matching text. Click **Search** there, or press Ctrl+F, to search text. **File** stays in the current file and **Global** covers the repo. Ctrl+F fills the query from the selection. Search skips `node_modules`, `.git`, and gitignored paths. Opening a file or switching tabs expands its folders in the explorer and selects it. Renaming a file there updates imports that point at it. Click **Help** in the status bar for the shortcut list. Click the theme name there to choose `p0`, `light`, `contrast`, `ocean`, or `ember`.

Claude is off until you turn it on. Open **Help** and click the Claude row. After that, click **Claude** in the status bar to show or hide the thread pane. Option+T anchors a conversation to the selection, or to the cursor line, and waits for Enter. Option+E asks for one edit instruction. A running turn can be stopped from that status segment or from **Stop** in the pane. When the turn finishes, the files it changed open. Claude has to be installed and logged in; the editor does not start a turn until you ask.

Right-click a symbol for its type, Go to Definition, and Find References. Click a gutter mark to show the previous lines, and click again to hide them. Click ↩ before the line number to revert the change. The cursor line ends with the last author's name in pale text, and ↗ opens that commit. A red underline is an error and a yellow one is a warning; the status bar shows that message at the cursor, and pausing the mouse shows the full text.

In a Git repository, click **Changes** on the right of the explorer border to stage, commit, pull, and push. Click the branch in the status bar to list local branches, and click one to switch. A name that is not in the list creates that branch. **Explorer** on the left of that border returns to the file tree. Click **Diff** on the right of the file border to open that same interactive diff for the current file. The file name on the left, or opening a file, returns to the buffer. The list is a folder tree. `+` stages a file or folder and `−` unstages it. Space does the same for the selected row. Stage and discard show while the pointer is on that row. A directory that is missing, and whose every change is a deletion, is one row. Restore brings that directory back. A footer stays at the bottom while the file list scrolls, and the last rows can scroll clear of it. **↓1 Pull** and **↑2 Push** are buttons, bright when that count is above zero. A line separates them from the subject, an optional body, and the **Commit** button. Opening that section lists recent commits on a short spine, and hovering one shows the rest of that commit. Enter in the subject commits. Commit writes only what is staged. **Pull** fast-forwards from upstream. **Push** sends the branch, and asks once before publishing a branch that has no upstream. The branch chip shows the same `main ↑2 ↓1` counts when they are known. A branch change or reset made outside the editor reloads clean open files. In that diff, **Stage**, **Unstage**, and **Discard** act on one hunk. Save the file before staging it. Delete, discard, restore, and closing or quitting with unsaved changes run immediately. A clean file changed on disk still asks before reload. A file git is still merging is listed under **Conflicts**. The conflict markers stay in the file and are not drawn. **Current** keeps your side, **Incoming** keeps the other side, and **Both** keeps your side and then the other. The status bar then says to save and stage the file. Staging marks it resolved. Commit finishes the merge, and stays off until every conflict is gone.

## Config

`~/Library/Application Support/r3vu/config.toml`

```toml
explorer_width = 28
tab_width = 4
explorer_visible = true
theme = "p0"
claude_enabled = false
```

## Optional

Types and completion use a language server. Editing continues if it is missing.

TypeScript and JavaScript:

```bash
npm install -g typescript typescript-language-server
```

Python:

```bash
npm install -g pyright
```

Java:

```bash
brew install jdtls
```

The Java server also needs a JDK 21 or newer.

## Benchmarks

```bash
cargo bench --bench editor
```

That runs the Criterion suite in `benches/editor.rs` on a release build. Keystroke and reparse times are one character inserted and then deleted, so reparse includes two incremental parses. Highlight covers about 60 visible lines. These medians are from one run on this machine:

| Work | 200 lines | 5,000 lines | 30,000 lines |
| --- | ---: | ---: | ---: |
| Keystroke | 3.1 µs | 3.7 µs | 3.7 µs |
| Reparse | 512 µs | 2.15 ms | 15.1 ms |
| Viewport highlight | 526 µs | 562 µs | 614 µs |
| Open file | 426 µs | 11.0 ms | 67.8 ms |
| Search in file | 10.8 µs | 264 µs | 1.55 ms |

Loading the tree-sitter grammars took 29.5 ms. Finding a file among 20,000 paths took 1.03 ms after a 1.75 ms index. Listing a 120-file workspace took 383 µs, and searching it took 1.80 ms. Parsing `git status` took 82 µs for 500 paths and 788 µs for 5,000. Plots land in `target/criterion/`.

## Limits

- Git marks need a worktree.
- JSON, YAML, HTML, CSS, Markdown, Python, and Java are colored. Markdown can be previewed with Ctrl+Shift+M. Types and completion are available for TypeScript, JavaScript, Python, and Java. Other files still edit and save.
