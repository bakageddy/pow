import { invoke } from "@tauri-apps/api/core"
import type { IPlan } from "@/interfaces"

export interface Insight { severity: "info" | "warning"; title: string; detail: string; nodeId: number | null }
export interface MetricChange { label: string; before: number | null; after: number | null; changePercent: number | null; unit: string; direction: "improved" | "regressed" | "unchanged" | "unavailable" | "neutral" }
export interface PlanComparison { metrics: MetricChange[]; nodeChanges: { nodeType: string; before: number; after: number }[] }

export interface Step {
  id: string
  windowId: string
  sql: string
  note: string
  plan: IPlan | null
  insights: Insight[]
  createdAt: string
}
export interface ExplainOptions { verbose: boolean; buffers: boolean; wal: boolean; timing: boolean; settings: boolean; costs: boolean; summary: boolean }
export interface QuickNote { id: string; windowId: string; stepId: string | null; body: string; createdAt: string }
export interface QueryWindow {
  id: string
  workflowId: string
  parentId: string | null
  title: string
  sql: string
  explainOptions: ExplainOptions
  steps: Step[]
  notes: QuickNote[]
}
export interface Workflow {
  id: string
  name: string
  parentId: string | null
  host: string
  port: number
  username: string
  database: string
  connected: boolean
  windows: QueryWindow[]
}
export interface ConnectionInfo { host: string; port: number; username: string; database: string }

export const api = {
  load: () => invoke<Workflow[]>("load_workspace"),
  databasePath: () => invoke<string>("workspace_database_path"),
  openDatabase: (path: string) => invoke<void>("open_workspace_database", { path }),
  compare: (baselineId: string, currentId: string) => invoke<PlanComparison>("compare_steps", { baselineId, currentId }),
  loadTheme: () => invoke<string>("load_theme"),
  saveTheme: (theme: string) => invoke<void>("save_theme", { theme }),
  createWorkflow: (name: string) => invoke<string>("create_workflow", { name }),
  forkWorkflow: (workflowId: string) => invoke<string>("fork_workflow", { workflowId }),
  deleteWorkflow: (workflowId: string) => invoke<void>("delete_workflow", { workflowId }),
  createWindow: (workflowId: string, title: string) => invoke<string>("create_window", { workflowId, title }),
  forkWindow: (windowId: string) => invoke<string>("fork_window", { windowId }),
  closeWindow: (windowId: string) => invoke<void>("close_window", { windowId }),
  saveDraft: (windowId: string, sql: string, title: string) => invoke<void>("save_draft", { windowId, sql, title }),
  saveOptions: (windowId: string, options: ExplainOptions) => invoke<void>("save_explain_options", { windowId, options }),
  addQuickNote: (windowId: string, stepId: string | null, body: string) => invoke<string>("add_quick_note", { windowId, stepId, body }),
  saveStep: (windowId: string, sql: string, note: string) => invoke<string>("save_step", { windowId, sql, note }),
  connect: (workflowId: string, info: ConnectionInfo, password: string) => invoke<void>("connect_workflow", { workflowId, info, password }),
  explain: (windowId: string, sql: string, note: string, analyze: boolean, options: ExplainOptions) => invoke<string>("run_explain", { windowId, sql, note, analyze, options }),
}
