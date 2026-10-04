import { PlanService } from "../src/services/plan-service"

// Compiled into the Rust binary, NOT into the Solid webview bundle.
// Return JSON so Rust can validate and persist the fully processed plan.
Object.assign(globalThis, {
  powParsePlan(raw: string, sql: string): string {
    const service = new PlanService()
    return JSON.stringify(service.createPlan("PostgreSQL plan", service.fromSource(raw), sql))
  },
})
