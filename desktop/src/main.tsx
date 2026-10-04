import { createEffect, createMemo, createSignal, For, Show, onCleanup, onMount, type Component } from "solid-js"
import { render } from "solid-js/web"
import { api, type ConnectionInfo, type ExplainOptions, type PlanComparison, type QueryWindow, type Step, type Workflow } from "./api"
import type { Node } from "@/interfaces"
import { PlanDiagram } from "./PlanDiagram"
import { themes, isThemeId, type ThemeId } from "./themes"
import "./style.css"

const format = (n: unknown, unit = "") => typeof n === "number" && Number.isFinite(n) ? `${new Intl.NumberFormat("en", { maximumFractionDigits: 2 }).format(n)}${unit}` : "—"
const message = (error: unknown) => error instanceof Error ? error.message : String(error)
const PlanView: Component<{ step: Step; steps: Step[] }> = (props) => {
  const [selected, setSelected] = createSignal<Node | null>(null)
  const [baselineId, setBaselineId] = createSignal("")
  const [comparison, setComparison] = createSignal<PlanComparison | null>(null)
  const [compareError, setCompareError] = createSignal("")
  const candidates = createMemo(() => props.steps.filter(s => s.plan && s.id !== props.step.id))
  createEffect(() => {
    setSelected(null)
    const index = props.steps.findIndex(s => s.id === props.step.id)
    setBaselineId(props.steps.slice(0, index).filter(s => s.plan).at(-1)?.id || candidates()[0]?.id || "")
  })
  createEffect(() => {
    const currentId = props.step.id
    const baseline = baselineId()
    setComparison(null); setCompareError("")
    if (!baseline) return
    let cancelled = false
    void api.compare(baseline, currentId).then(value => { if (!cancelled) setComparison(value) }).catch(e => { if (!cancelled) setCompareError(message(e)) })
    onCleanup(() => { cancelled = true })
  })
  return <div class="plan-view">
    <Show when={props.step.plan}>
      {p => <>
        <div class="plan-summary">
          <div><small>EXECUTION</small><strong>{format(p().content["Execution Time"], " ms")}</strong></div>
          <div><small>PLANNING</small><strong>{format(p().content["Planning Time"], " ms")}</strong></div>
          <div><small>MAX COST</small><strong>{format(p().content.maxTotalCost)}</strong></div>
          <div><small>EST. ROWS</small><strong>{format(p().content.Plan["Plan Rows"])}</strong></div>
        </div>
        <PlanDiagram plan={p()} selected={selected()?.nodeId || null} onSelect={setSelected} />
        <div class="analysis-section">
          <div class="analysis-title">Plan observations <small>Heuristics · review with workload context</small></div>
          <Show when={props.step.insights.length} fallback={<div class="analysis-empty">No thresholds triggered. Inspect the diagram for details.</div>}>
            <For each={props.step.insights}>{insight => <div class={`insight ${insight.severity}`}><span class="insight-indicator" /><div><strong>{insight.title}</strong><span>{insight.detail}</span></div></div>}</For>
          </Show>
        </div>
        <div class="analysis-section comparison-section">
          <div class="analysis-title">Compare steps <small>Based on saved PostgreSQL plans</small></div>
          <Show when={candidates().length} fallback={<div class="analysis-empty">Capture another plan in this tab to compare revisions.</div>}>
            <label class="baseline-label">Baseline <select aria-label="Baseline plan step" value={baselineId()} onChange={e => setBaselineId(e.currentTarget.value)}>
              <For each={candidates()}>{candidate => <option value={candidate.id}>Step {props.steps.indexOf(candidate) + 1} · {candidate.note || "Captured plan"}</option>}</For>
            </select></label>
            <Show when={compareError()}><div class="comparison-error">{compareError()}</div></Show>
            <Show when={comparison()} fallback={<Show when={!compareError()}><div class="analysis-empty">Comparing saved plans…</div></Show>}>
              {result => <><div class="comparison-grid"><For each={result().metrics}>{metric => <div class="comparison-metric"><small>{metric.label}</small><strong>{format(metric.after, metric.unit ? ` ${metric.unit}` : "")}</strong><span class={`delta ${metric.direction}`}>{metric.changePercent === null ? "No comparable data" : `${metric.changePercent > 0 ? "+" : ""}${format(metric.changePercent)}% vs baseline`}</span></div>}</For></div>
                <Show when={result().nodeChanges.length}><div class="node-changes"><span>Plan shape</span><For each={result().nodeChanges}>{node => <span class="node-change">{node.nodeType}: {node.before} → {node.after}</span>}</For></div></Show>
              </>}
            </Show>
          </Show>
        </div>
      </>}
    </Show>
  </div>
}

const App: Component = () => {
  const [workflows, setWorkflows] = createSignal<Workflow[]>([])
  const [activeId, setActiveId] = createSignal<string | null>(null)
  const [leftId, setLeftId] = createSignal<string | null>(null)
  const [rightId, setRightId] = createSignal<string | null>(null)
  const [split, setSplit] = createSignal(false)
  const [drafts, setDrafts] = createSignal<Record<string, string>>({})
  const [titles, setTitles] = createSignal<Record<string, string>>({})
  const [notes, setNotes] = createSignal<Record<string, string>>({})
  const [optionsEdits, setOptionsEdits] = createSignal<Record<string, ExplainOptions>>({})
  const [editorCollapsed, setEditorCollapsed] = createSignal<Record<string, boolean>>({})
  const [activePane, setActivePane] = createSignal<"left" | "right">("left")
  const [quickNoteContext, setQuickNoteContext] = createSignal<{ windowId: string; stepId: string | null } | null>(null)
  const [selections, setSelections] = createSignal<Record<string, string>>({})
  const [connectionOpen, setConnectionOpen] = createSignal(false)
  const [databaseOpen, setDatabaseOpen] = createSignal(false)
  const [workspacePath, setWorkspacePath] = createSignal("")
  const [createOpen, setCreateOpen] = createSignal(false)
  const [confirmAction, setConfirmAction] = createSignal<{ title: string; description: string; label: string; run: () => void } | null>(null)
  const [theme, setTheme] = createSignal<ThemeId>("nord")
  const [themeOpen, setThemeOpen] = createSignal(false)
  createEffect(() => { document.documentElement.dataset.theme = theme() })
  void api.loadTheme().then(id => { if (isThemeId(id)) setTheme(id) }).catch(e => setError(message(e)))
  function chooseTheme(id: ThemeId) {
    setTheme(id); setThemeOpen(false)
    void api.saveTheme(id).catch(e => setError(message(e)))
  }
  const [busy, setBusy] = createSignal(false)
  const [error, setError] = createSignal("")
  const [toast, setToast] = createSignal("")
  const current = createMemo(() => workflows().find(w => w.id === activeId()))
  const timers = new Map<string, ReturnType<typeof setTimeout>>()
  function openQuickNote(side = activePane()) {
    const windowId = side === "left" ? leftId() : rightId()
    const win = current()?.windows.find(w => w.id === windowId)
    if (!win) return
    const stepId = win.steps.find(s => s.id === selections()[win.id])?.id || win.steps.at(-1)?.id || null
    setQuickNoteContext({ windowId: win.id, stepId })
  }
  onMount(() => {
    const shortcut = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.code === "Backslash") {
        e.preventDefault()
        if (!quickNoteContext() && !connectionOpen() && !databaseOpen() && !createOpen() && !confirmAction()) openQuickNote()
      }
    }
    window.addEventListener("keydown", shortcut)
    onCleanup(() => window.removeEventListener("keydown", shortcut))
  })
  async function saveQuickNote(body: string) {
    const target = quickNoteContext()
    if (!target) return
    await action(async () => {
      await api.addQuickNote(target.windowId, target.stepId, body)
      await reload()
      setQuickNoteContext(null)
      notify("Note saved")
    })
  }

  async function reload() {
    const data = await api.load()
    setWorkflows(data)
    if (!data.some(w => w.id === activeId())) {
      setActiveId(data[0]?.id || null)
      setLeftId(data[0]?.windows[0]?.id || null)
      setRightId(data[0]?.windows[1]?.id || data[0]?.windows[0]?.id || null)
    }
  }
  void reload().catch(e => setError(message(e)))
  void api.databasePath().then(setWorkspacePath).catch(e => setError(message(e)))

  const action = async (job: () => Promise<void>) => {
    setBusy(true); setError("")
    try { await job() } catch (e) { setError(message(e)) } finally { setBusy(false) }
  }
  const notify = (text: string) => { setToast(text); setTimeout(() => setToast(""), 3200) }
  async function openDatabase(path: string) {
    await action(async () => {
      for (const workflow of workflows()) {
        for (const win of workflow.windows) {
          if (Object.hasOwn(drafts(), win.id) || Object.hasOwn(titles(), win.id)) await persist(win)
        }
      }
      await api.openDatabase(path)
      setWorkflows([]); setActiveId(null); setLeftId(null); setRightId(null); setSplit(false)
      setDrafts({}); setTitles({}); setNotes({}); setSelections({}); setOptionsEdits({}); setEditorCollapsed({})
      setWorkspacePath(await api.databasePath())
      const newTheme = await api.loadTheme()
      if (isThemeId(newTheme)) setTheme(newTheme)
      await reload(); setDatabaseOpen(false); notify("Workspace database opened")
    })
  }
  function chooseWorkflow(id: string) {
    setActiveId(id)
    const w = workflows().find(w => w.id === id)
    setLeftId(w?.windows[0]?.id || null)
    setRightId(w?.windows[1]?.id || w?.windows[0]?.id || null)
    setSplit(false)
  }
  async function createWorkflow(name: string) {
    await action(async () => { const id = await api.createWorkflow(name); await reload(); chooseWorkflow(id); setCreateOpen(false); setConnectionOpen(true) })
  }
  async function addWindow(side: "left" | "right") {
    if (!current()) return
    await action(async () => {
      const id = await api.createWindow(current()!.id, `Query ${current()!.windows.length + 1}`)
      await reload(); (side === "left" ? setLeftId : setRightId)(id)
    })
  }
  async function forkWindow(win: QueryWindow, side: "left" | "right") {
    await action(async () => {
      await persist(win)
      await api.saveOptions(win.id, optionsFor(win))
      const id = await api.forkWindow(win.id)
      await reload(); (side === "left" ? setLeftId : setRightId)(id); notify("Query window forked with its history")
    })
  }
  function updateDraft(win: QueryWindow, sql: string) {
    setDrafts(d => ({ ...d, [win.id]: sql }))
    clearTimeout(timers.get(win.id))
    timers.set(win.id, setTimeout(() => void persist(win).catch(e => setError(message(e))), 650))
  }
  async function persist(win: QueryWindow) {
    clearTimeout(timers.get(win.id))
    await api.saveDraft(win.id, drafts()[win.id] ?? win.sql, titles()[win.id] ?? win.title)
  }
  const sqlFor = (win: QueryWindow) => drafts()[win.id] ?? win.sql
  const noteFor = (win: QueryWindow) => notes()[win.id] ?? ""
  const optionsFor = (win: QueryWindow) => optionsEdits()[win.id] ?? win.explainOptions
  function toggleOption(win: QueryWindow, key: keyof ExplainOptions) {
    const updated = { ...optionsFor(win), [key]: !optionsFor(win)[key] }
    setOptionsEdits(all => ({ ...all, [win.id]: updated }))
    void api.saveOptions(win.id, updated).catch(e => setError(message(e)))
  }
  async function capture(win: QueryWindow, analyze: boolean) {
    if (analyze) {
      setConfirmAction({ title: "Run EXPLAIN ANALYZE?", description: "This executes the query. Pow uses a read-only transaction, rolls it back, and applies a 15-second timeout. External side effects from functions are still possible.", label: "Run analyze", run: () => void executeCapture(win, true) })
      return
    }
    await executeCapture(win, false)
  }
  async function executeCapture(win: QueryWindow, analyze: boolean) {
    await action(async () => {
      const id = await api.explain(win.id, sqlFor(win), noteFor(win), analyze, optionsFor(win))
      clearTimeout(timers.get(win.id))
      setSelections(s => ({ ...s, [win.id]: id })); setNotes(n => ({ ...n, [win.id]: "" }))
      await reload(); setEditorCollapsed(all => ({ ...all, [win.id]: true })); notify("Plan captured as a new step")
    })
  }
  async function snapshot(win: QueryWindow) {
    await action(async () => {
      const id = await api.saveStep(win.id, sqlFor(win), noteFor(win))
      clearTimeout(timers.get(win.id))
      setSelections(s => ({ ...s, [win.id]: id })); setNotes(n => ({ ...n, [win.id]: "" }))
      await reload(); notify("Step saved")
    })
  }
  function pane(side: "left" | "right") {
    const id = () => side === "left" ? leftId() : rightId()
    const select = side === "left" ? setLeftId : setRightId
    const win = () => current()?.windows.find(w => w.id === id())
    const step = () => win()?.steps.find(s => s.id === selections()[win()!.id]) || win()?.steps.at(-1)
    return <section class="pane" onPointerDown={() => setActivePane(side)}>
      <div class="tabbar"><div class="tabs"><For each={current()?.windows || []}>{w => <button class={`tab ${id() === w.id ? "active" : ""}`} onClick={() => select(w.id)} title={w.title}><span class="tab-dot" />{titles()[w.id] || w.title}</button>}</For></div>
        <button class="icon-btn" aria-label="New query window" title="New query window" onClick={() => void addWindow(side)}>＋</button>
      </div>
      <Show when={win()} fallback={<div class="empty-pane"><div class="empty-glyph">{`>_`}</div><h2>No query tabs yet</h2><p>Create a tab to write SQL and capture a plan.</p><button class="primary" onClick={() => void addWindow(side)}>New query tab</button></div>}>
        {w => <>
          <div class="pane-toolbar"><div class="crumb">{current()?.name} <span> / </span> <input aria-label="Query window title" value={titles()[w().id] ?? w().title} onInput={e => { setTitles(t => ({ ...t, [w().id]: e.currentTarget.value })); updateDraft(w(), sqlFor(w())) }} onBlur={() => void persist(w()).catch(e => setError(message(e)))} /></div>
            <div class="toolbar-actions"><button class="subtle" title="Quick note (Ctrl+\\)" onClick={() => openQuickNote(side)}>Note <kbd>Ctrl+\\</kbd></button><button class="subtle" title="Fork this query with all saved steps" onClick={() => void forkWindow(w(), side)}>⑂ Fork</button><button class="subtle" title="Close query window" onClick={() => setConfirmAction({ title: `Close ${w().title}?`, description: "This removes the tab and all of its saved steps. This cannot be undone.", label: "Close tab", run: () => void action(async () => { await api.closeWindow(w().id); await reload(); select(current()?.windows[0]?.id || null) }) })}>×</button></div>
          </div>
          <div class="pane-scroll"><div class="editor-heading"><div><span class="section-label">Query</span><span class="editor-status">Draft auto-saved</span></div><button class="editor-toggle" aria-expanded={!editorCollapsed()[w().id]} onClick={() => setEditorCollapsed(all => ({ ...all, [w().id]: !all[w().id] }))}>{editorCollapsed()[w().id] ? "▾ Show editor" : "▴ Hide editor"}</button></div>
            <Show when={!editorCollapsed()[w().id]} fallback={<button class="query-preview" title="Show SQL editor" onClick={() => setEditorCollapsed(all => ({ ...all, [w().id]: false }))}><span>SQL</span> {sqlFor(w()).trim().split("\n")[0] || "Empty query"}</button>}>
              <div class="editor"><div class="line-gutter">{Array.from({ length: Math.max(8, sqlFor(w()).split("\n").length) }, (_, i) => <div>{i + 1}</div>)}</div><textarea spellcheck={false} aria-label="SQL query" placeholder="SELECT * FROM your_table WHERE ..." value={sqlFor(w())} onInput={e => updateDraft(w(), e.currentTarget.value)} onBlur={() => void persist(w()).catch(e => setError(message(e)))} /></div>
              <div class="explain-settings"><div class="section-label">EXPLAIN options</div><div class="option-buttons"><For each={["verbose", "buffers", "wal", "timing", "settings", "costs", "summary"] as (keyof ExplainOptions)[]}>{key => <button class={optionsFor(w())[key] ? "enabled" : ""} aria-pressed={optionsFor(w())[key]} title={["buffers", "wal", "timing", "summary"].includes(key) ? "Only applies to Explain analyze" : `Toggle ${key}`} onClick={() => toggleOption(w(), key)}>{key}</button>}</For></div><small>Buffers, WAL, timing &amp; summary apply to Analyze.</small></div>
              <div class="note-row"><input aria-label="Note for next step" placeholder="What are you changing? Add a note for the next step…" value={noteFor(w())} onInput={e => setNotes(n => ({ ...n, [w().id]: e.currentTarget.value }))} /></div>
              <div class="runbar"><div class="run-buttons"><button class="primary" disabled={busy()} onClick={() => void capture(w(), false)}>Explain plan</button><button class="secondary" disabled={busy()} title="Runs the query inside a read-only transaction" onClick={() => void capture(w(), true)}>Explain analyze</button></div><button class="subtle" disabled={busy()} onClick={() => void snapshot(w())}>Save step</button></div>
            </Show>
            <div class="section-divider"><span>History</span><small>{w().steps.length} {w().steps.length === 1 ? "step" : "steps"}</small></div>
            <div class="step-list"><For each={w().steps}>{(s, i) => <button class={`step-chip ${step()?.id === s.id ? "active" : ""}`} onClick={() => setSelections(v => ({ ...v, [w().id]: s.id }))}><small>{String(i() + 1).padStart(2, "0")}</small>{s.note || (s.plan ? "Captured plan" : "Saved revision")}{s.plan ? <span class="plan-indicator">●</span> : null}</button>}</For></div>
            <Show when={step()} fallback={<div class="empty-plan"><div class="empty-glyph">{`>_`}</div><h3>No plan captured</h3><p>Run Explain to inspect how PostgreSQL would execute this query.</p></div>}>
              {s => <><div class="result-heading"><span class="section-label">Step {w().steps.indexOf(s()) + 1} / {s().note || "Plan"}</span><button class="subtle" title="Replace the draft with this step's SQL" onClick={() => updateDraft(w(), s().sql)}>Restore SQL</button></div>
                <Show when={s().plan} fallback={<div class="empty-plan compact">This revision has no plan yet. Explain the query to capture one.</div>}><PlanView step={s()} steps={w().steps} /></Show>
              </>}
            </Show>
            <Show when={w().notes.length}><div class="quick-notes"><div class="section-divider"><span>Quick notes</span><small>{w().notes.length}</small></div><For each={w().notes}>{note => <div class="quick-note-entry"><div><Show when={note.stepId}><button class="note-step" onClick={() => setSelections(all => ({ ...all, [w().id]: note.stepId! }))}>Step {w().steps.findIndex(s => s.id === note.stepId) + 1}</button></Show><small>{note.createdAt}</small></div><p>{note.body}</p></div>}</For></div></Show>
          </div>
        </>}
      </Show>
    </section>
  }

  return <div class="app-shell">
    <aside class="sidebar"><div class="traffic"><i /><i /><i /></div><div class="brand"><div class="brand-mark">p</div><div><strong>pow</strong><small>Query Studio</small></div></div>
      <div class="side-heading"><span>Pinned workflows</span><button class="icon-btn" aria-label="Create workflow" title="Create workflow" onClick={() => setCreateOpen(true)}>+</button></div>
      <nav class="workflow-list"><For each={workflows()}>{w => <button class={`workflow-item ${activeId() === w.id ? "active" : ""}`} onClick={() => chooseWorkflow(w.id)} title={w.name}><span class="pin">{w.name.slice(0, 1).toUpperCase()}</span><span class="workflow-name">{w.name}<small>{w.windows.length} {w.windows.length === 1 ? "tab" : "tabs"}</small></span><span class={`connection-dot ${w.connected ? "online" : ""}`} /></button>}</For></nav>
      <div class="sidebar-bottom"><button class="workspace-file" title={workspacePath()} onClick={() => setDatabaseOpen(true)}>Database · {workspacePath().split(/[\\/]/).at(-1) || "Loading…"}</button><div class="sidebar-footer">Local workspace <span>v0.3.0</span></div></div>
    </aside>
    <main class="main"><header class="topbar"><div class="topbar-title"><div><small>WORKSPACE / {current() ? "WORKFLOW" : "HOME"}</small><strong>{current()?.name || "Pow"}</strong></div></div><div class="topbar-actions"><Show when={current()}>{w => <><button class="top-button connection-button" onClick={() => setConnectionOpen(true)}><span class={`connection-dot ${w().connected ? "online" : ""}`} />{w().connected ? `${w().database}@${w().host}` : "Connect database"}</button><button class="top-button" onClick={() => void action(async () => { const id = await api.forkWorkflow(w().id); await reload(); chooseWorkflow(id); notify("Workflow forked") })}>Fork</button><button class={`top-button ${split() ? "pressed" : ""}`} title="Toggle split view" onClick={() => { if (!split()) setRightId(rightId() || current()?.windows.find(q => q.id !== leftId())?.id || leftId()); setSplit(v => !v) }}>▣ Split</button><button class="icon-btn danger" title="Delete workflow" aria-label="Delete workflow" onClick={() => setConfirmAction({ title: `Delete ${w().name}?`, description: "All tabs, steps, SQL drafts, and plans in this workflow will be permanently removed.", label: "Delete workflow", run: () => void action(async () => { await api.deleteWorkflow(w().id); await reload() }) })}>×</button></>}</Show>
        <div class="theme-holder"><button class="top-button theme-trigger" aria-label="Choose color theme" aria-expanded={themeOpen()} onClick={() => setThemeOpen(!themeOpen())}><span class="theme-swatch" />Theme <span class="chevron">⌄</span></button>
          <Show when={themeOpen()}><><div class="theme-dismiss" onClick={() => setThemeOpen(false)} /><div class="theme-menu" role="group" aria-label="Color theme"><div class="theme-menu-title">Appearance <span>{themes.length} themes</span></div><div class="theme-options"><For each={themes}>{option => <button class={`theme-option ${theme() === option.id ? "active" : ""}`} aria-label={`Use ${option.name} theme`} aria-pressed={theme() === option.id} onClick={() => chooseTheme(option.id)}><span class="palette-preview" style={{ background: option.swatches[0] }}><i style={{ background: option.swatches[1] }} /><i style={{ background: option.swatches[2] }} /></span><span>{option.name}</span><span class="theme-check">{theme() === option.id ? "✓" : ""}</span></button>}</For></div></div></></Show>
        </div></div></header>
      <Show when={current()} fallback={<div class="welcome"><div class="welcome-mark">p_</div><div class="eyebrow">POSTGRESQL / QUERY STUDIO</div><h1>Query plans,<br/>without the clutter.</h1><p>Create a workflow, open a query tab, and keep each revision and execution plan in one place.</p><button class="primary large" onClick={() => setCreateOpen(true)}>New workflow <span>↗</span></button><div class="welcome-foot">Private by default · Stored on this device</div></div>}>
        <div class={`work-area ${split() ? "is-split" : ""}`}>{pane("left")}<Show when={split()}><div class="split-handle" />{pane("right")}</Show></div>
      </Show>
    </main>
    <Show when={databaseOpen()}><WorkspaceDatabaseDialog path={workspacePath()} close={() => setDatabaseOpen(false)} open={path => void openDatabase(path)} busy={busy()} /></Show>
    <Show when={quickNoteContext()}>{scope => <QuickNoteDialog scope={scope().stepId ? "Current step" : "Query tab"} close={() => setQuickNoteContext(null)} save={body => void saveQuickNote(body)} busy={busy()} />}</Show>
    <Show when={createOpen()}><CreateWorkflowDialog close={() => setCreateOpen(false)} create={name => void createWorkflow(name)} busy={busy()} /></Show>
    <Show when={confirmAction()}>{decision => <ConfirmDialog title={decision().title} description={decision().description} label={decision().label} close={() => setConfirmAction(null)} confirm={() => { const run = decision().run; setConfirmAction(null); run() }} />}</Show>
    <Show when={connectionOpen() && current()}>{w => <ConnectionDialog workflow={w()} close={() => setConnectionOpen(false)} connected={() => void action(async () => { await reload(); setConnectionOpen(false); notify("Connected to PostgreSQL") })} fail={setError} />}</Show>
    <Show when={error()}><div class="error-toast" role="alert"><span>!</span>{error()}<button onClick={() => setError("")}>×</button></div></Show>
    <Show when={toast()}><div class="success-toast" role="status">✓ {toast()}</div></Show>
  </div>
}

const WorkspaceDatabaseDialog: Component<{ path: string; close: () => void; open: (path: string) => void; busy: boolean }> = props => {
  const [path, setPath] = createSignal(props.path)
  return <div class="modal-backdrop" onMouseDown={e => { if (e.target === e.currentTarget) props.close() }}><form class="connection-modal" onSubmit={e => { e.preventDefault(); if (path().trim()) props.open(path().trim()) }}>
    <div class="modal-kicker">WORKSPACE DATABASE</div><h2>Open a shared workspace</h2><p>Enter the absolute path to an existing Pow Turso database file. This opens it in place; the current database is left untouched. Passwords must be re-entered.</p>
    <label>Database file path<input required value={path()} onInput={e => setPath(e.currentTarget.value)} placeholder="/path/to/pow.turso" /></label>
    <div class="privacy-note">When sharing, close Pow before copying its database file. Include any associated WAL file or checkpoint first. Do not put a live database on a network filesystem that lacks reliable locking.</div>
    <div class="modal-actions"><button class="secondary" type="button" onClick={props.close}>Cancel</button><button class="primary" type="submit" disabled={props.busy || !path().trim()}>Open database</button></div>
  </form></div>
}

const QuickNoteDialog: Component<{ scope: string; close: () => void; save: (body: string) => void; busy: boolean }> = props => {
  const [body, setBody] = createSignal("")
  return <div class="modal-backdrop" onMouseDown={e => { if (e.target === e.currentTarget) props.close() }}>
    <form class="connection-modal quick-note-dialog" onSubmit={e => { e.preventDefault(); if (body().trim()) props.save(body().trim()) }} onKeyDown={e => {
      if (e.key === "Escape") props.close()
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); if (body().trim()) props.save(body().trim()) }
    }}>
      <div class="modal-kicker">QUICK NOTE · {props.scope}</div><h2>Capture an observation</h2><p>Stored in the workspace database. Ctrl+Enter to save.</p>
      <label>Note<textarea autofocus required maxlength="20000" rows="5" value={body()} onInput={e => setBody(e.currentTarget.value)} placeholder="What did you notice in this plan?" /></label>
      <div class="modal-actions"><button type="button" class="secondary" onClick={props.close}>Cancel</button><button type="submit" class="primary" disabled={props.busy || !body().trim()}>Save note</button></div>
    </form>
  </div>
}

const CreateWorkflowDialog: Component<{ close: () => void; create: (name: string) => void; busy: boolean }> = props => {
  const [name, setName] = createSignal("")
  return <div class="modal-backdrop" onMouseDown={e => { if (e.target === e.currentTarget) props.close() }}><form class="connection-modal" onSubmit={e => { e.preventDefault(); if (name().trim()) props.create(name().trim()) }}>
    <div class="modal-kicker">NEW WORKFLOW</div><h2>Create a workflow</h2><p>Keep related query tabs and their plan history together.</p>
    <label>Workflow name<input autofocus required maxlength="80" value={name()} onInput={e => setName(e.currentTarget.value)} placeholder="e.g. Reporting queries" /></label>
    <div class="modal-actions"><button type="button" class="secondary" onClick={props.close}>Cancel</button><button class="primary" type="submit" disabled={props.busy || !name().trim()}>Create workflow</button></div>
  </form></div>
}

const ConfirmDialog: Component<{ title: string; description: string; label: string; close: () => void; confirm: () => void }> = props => (
  <div class="modal-backdrop" onMouseDown={e => { if (e.target === e.currentTarget) props.close() }}><div class="connection-modal confirm-modal" role="alertdialog" aria-modal="true">
    <div class="modal-kicker">CONFIRM ACTION</div><h2>{props.title}</h2><p>{props.description}</p><div class="modal-actions"><button class="secondary" onClick={props.close}>Cancel</button><button class="primary" onClick={props.confirm}>{props.label}</button></div>
  </div></div>
)

const ConnectionDialog: Component<{ workflow: Workflow; close: () => void; connected: () => void; fail: (error: string) => void }> = props => {
  const [host, setHost] = createSignal(props.workflow.host || "localhost")
  const [port, setPort] = createSignal(props.workflow.port || 5432)
  const [username, setUsername] = createSignal(props.workflow.username || "postgres")
  const [database, setDatabase] = createSignal(props.workflow.database || "postgres")
  const [password, setPassword] = createSignal("")
  const [connecting, setConnecting] = createSignal(false)
  async function connect(e: SubmitEvent) {
    e.preventDefault(); setConnecting(true)
    const info: ConnectionInfo = { host: host().trim(), port: port(), username: username().trim(), database: database().trim() }
    try { await api.connect(props.workflow.id, info, password()); setPassword(""); props.connected() }
    catch (e) { props.fail(message(e)) } finally { setConnecting(false) }
  }
  return <div class="modal-backdrop" onMouseDown={e => { if (e.target === e.currentTarget) props.close() }}><form class="connection-modal" onSubmit={e => void connect(e)}><div class="modal-kicker">POSTGRESQL CONNECTION</div><h2>Connect to PostgreSQL</h2><p>Point this workflow at a database to capture real execution plans.</p>
    <label>Hostname<input required value={host()} onInput={e => setHost(e.currentTarget.value)} placeholder="localhost" /></label><div class="form-pair"><label>Port<input required type="number" min="1" max="65535" value={port()} onInput={e => setPort(Number(e.currentTarget.value))} /></label><label>Database<input required value={database()} onInput={e => setDatabase(e.currentTarget.value)} /></label></div>
    <label>Username<input required value={username()} onInput={e => setUsername(e.currentTarget.value)} /></label><label>Password<input type="password" autocomplete="off" value={password()} onInput={e => setPassword(e.currentTarget.value)} placeholder="••••••••" /></label><div class="privacy-note">The password stays in memory for this session. This build does not support TLS: use a local server or a trusted tunnel.</div>
    <div class="modal-actions"><button type="button" class="secondary" onClick={props.close}>Cancel</button><button class="primary" disabled={connecting()} type="submit">{connecting() ? "Connecting…" : "Connect database"}</button></div></form></div>
}

render(() => <App />, document.getElementById("root")!)
