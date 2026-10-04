import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, type Component } from "solid-js"
import * as d3 from "d3"
import { flextree, type FlexHierarchyPointNode } from "d3-flextree"
import type { IPlan, Node } from "@/interfaces"

const CARD_WIDTH = 288
const NODE_GAP = 55
const cardHeight = (expanded: boolean) => expanded ? 182 : 121
const numeric = (node: Node, key: string): number | undefined => {
  const value = node[key]
  return typeof value === "number" && Number.isFinite(value) ? value : undefined
}
const formatted = (value: number | undefined, unit = "") => value === undefined ? "—" : `${new Intl.NumberFormat("en", { maximumFractionDigits: 2 }).format(value)}${unit}`
const title = (node: Node) => [node["Parallel Aware"] ? "Parallel" : "", node["Partial Mode"] || "", node["Node Type"] || "Operation"].filter(Boolean).join(" ")

type Point = FlexHierarchyPointNode<Node>
type Edge = { source: Point; target: Point; cte?: boolean }
type Box = { x: number; y: number; width: number; height: number; label: string }
function extent(nodes: Point[], height: (node: Point) => number) {
  return {
    left: Math.min(...nodes.map(node => node.x - CARD_WIDTH / 2)),
    right: Math.max(...nodes.map(node => node.x + CARD_WIDTH / 2)),
    top: Math.min(...nodes.map(node => node.y)),
    bottom: Math.max(...nodes.map(node => node.y + height(node))),
  }
}

export const PlanDiagram: Component<{ plan: IPlan; selected: number | null; onSelect: (node: Node | null) => void }> = props => {
  let canvas!: SVGSVGElement
  let viewport!: HTMLDivElement
  const [expanded, setExpanded] = createSignal<Set<number>>(new Set())
  const [metric, setMetric] = createSignal<"duration" | "cost" | "rows">(props.plan.isAnalyze ? "duration" : "cost")
  const [transform, setTransform] = createSignal(d3.zoomIdentity)
  const [scale, setScale] = createSignal(1)
  const isExpanded = (id: number) => expanded().has(id)
  const height = (point: Point) => cardHeight(isExpanded(point.data.nodeId))

  // The same d3-flextree layout and d3 zoom used by PEV2's Plan.vue.
  const diagram = createMemo(() => {
    const layout = flextree<Node>({
      nodeSize: node => [CARD_WIDTH + 25, cardHeight(isExpanded(node.data.nodeId)) + NODE_GAP],
      spacing: (a, b) => Math.pow(a.path(b).length, 1.5),
    })
    const main = layout(layout.hierarchy(props.plan.content.Plan, node => node.Plans || []))
    const nodes: Point[] = [...main.descendants()]
    const edges: Edge[] = main.links()
    const boxes: Box[] = []
    const mainExtent = extent(nodes, height)
    let nextX = mainExtent.left
    const cteRoots: Point[] = []
    for (const cte of props.plan.ctes || []) {
      const root = layout(layout.hierarchy(cte, node => node.Plans || []))
      const descendants = root.descendants()
      const box = extent(descendants, height)
      const shiftX = nextX - box.left + 15
      const shiftY = mainExtent.bottom - box.top + 100
      descendants.forEach(node => { node.x += shiftX; node.y += shiftY })
      nodes.push(...descendants)
      edges.push(...root.links())
      boxes.push({ x: box.left + shiftX - 12, y: box.top + shiftY - 30, width: box.right - box.left + 24, height: box.bottom - box.top + 44, label: String(cte["Subplan Name"] || "CTE") })
      cteRoots.push(root)
      nextX += box.right - box.left + 75
    }
    // Keep CTE references visible where PEV2 supplies a matching CTE name.
    for (const node of nodes) {
      const name = node.data["CTE Name"]
      if (typeof name === "string") {
        const target = cteRoots.find(root => root.data["Subplan Name"] === `CTE ${name}`)
        if (target) edges.push({ source: node, target, cte: true })
      }
    }
    const bounds = extent(nodes, height)
    return { nodes, edges, boxes, bounds }
  })

  const inspected = createMemo(() => diagram().nodes.find(point => point.data.nodeId === props.selected)?.data)
  const zoom = d3.zoom<SVGSVGElement, unknown>()
    .scaleExtent([0.2, 3])
    .filter(event => event.type === "wheel" || (event.button === 0 && !(event.target as Element).closest("[data-plan-card]")))
    .on("zoom", event => { setTransform(event.transform); setScale(event.transform.k) })

  function fit() {
    if (!canvas || !viewport) return
    const { left, right, top, bottom } = diagram().bounds
    const width = viewport.clientWidth
    const height = viewport.clientHeight
    if (!width || !height) return
    const k = Math.max(.2, Math.min(1.3, .86 * width / Math.max(1, right - left), .86 * height / Math.max(1, bottom - top)))
    const centerX = (left + right) / 2
    const centerY = (top + bottom) / 2
    d3.select(canvas).call(zoom.transform, d3.zoomIdentity.translate(width / 2 - centerX * k, height / 2 - centerY * k).scale(k))
  }
  function zoomBy(factor: number) {
    d3.select(canvas).transition().duration(180).call(zoom.scaleBy, factor)
  }
  function toggle(id: number) {
    setExpanded(old => { const next = new Set(old); next.has(id) ? next.delete(id) : next.add(id); return next })
  }
  const value = (node: Node) => {
    if (metric() === "cost") return numeric(node, "*Cost (exclusive)") ?? numeric(node, "Total Cost")
    if (metric() === "rows") return numeric(node, "*Actual Rows Revised") ?? numeric(node, "Plan Rows")
    return numeric(node, "*Duration (exclusive)")
  }
  const maxMetric = () => {
    if (metric() === "duration") return props.plan.content.maxDuration || 0
    if (metric() === "cost") return props.plan.content.maxCost || 0
    return Math.max(...diagram().nodes.map(point => value(point.data) || 0), 1)
  }
  const barPercent = (node: Node) => Math.min(100, Math.max(0, (value(node) || 0) / (maxMetric() || 1) * 100))
  const edgePath = (edge: Edge) => {
    const fromY = edge.source.y + height(edge.source)
    const toY = edge.target.y
    const curve = d3.path()
    curve.moveTo(edge.source.x, fromY)
    curve.bezierCurveTo(edge.source.x, fromY + (toY - fromY) / 2, edge.target.x, toY - (toY - fromY) / 2, edge.target.x, toY)
    return curve.toString()
  }
  const edgeWidth = (node: Node) => {
    const max = props.plan.content.maxRows || 0
    const rows = numeric(node, "*Actual Rows Revised") || 0
    return max > 0 ? Math.max(1, Math.min(18, rows / max * 18)) : 1.3
  }

  onMount(() => {
    d3.select(canvas).call(zoom)
    const observer = new ResizeObserver(() => fit())
    observer.observe(viewport)
    requestAnimationFrame(fit)
    onCleanup(() => { observer.disconnect(); d3.select(canvas).on(".zoom", null) })
  })
  createEffect(() => { props.plan.id; requestAnimationFrame(fit) })
  createEffect(() => { props.plan.id; setMetric(props.plan.isAnalyze ? "duration" : "cost"); setExpanded(new Set<number>()) })

  return <div class="diagram-shell">
    <div class="diagram-toolbar"><div><strong>Execution graph</strong><span>Drag to pan · scroll to zoom</span></div><div class="diagram-controls">
      <div class="metric-switch"><For each={["duration", "rows", "cost"] as const}>{item => <button class={metric() === item ? "active" : ""} disabled={item === "duration" && !props.plan.isAnalyze} onClick={() => setMetric(item)}>{item}</button>}</For></div>
      <button aria-label="Zoom out" title="Zoom out" onClick={() => zoomBy(.8)}>−</button><span class="zoom-label">{Math.round(scale() * 100)}%</span><button aria-label="Zoom in" title="Zoom in" onClick={() => zoomBy(1.25)}>+</button><button title="Fit diagram" onClick={fit}>Fit</button>
    </div></div>
    <div class="diagram-viewport" ref={viewport}><svg ref={canvas} role="img" aria-label="Interactive PostgreSQL execution plan diagram" width="100%" height="100%">
      <g transform={transform().toString()}>
        <For each={diagram().boxes}>{box => <g><rect class="cte-outline" x={box.x} y={box.y} width={box.width} height={box.height} rx="8" /><text class="cte-label" x={box.x + 12} y={box.y + 19}>{box.label}</text></g>}</For>
        <For each={diagram().edges}>{edge => <path class={edge.cte ? "diagram-edge cte-edge" : "diagram-edge"} d={edgePath(edge)} stroke-width={edge.cte ? 2 : edgeWidth(edge.target.data)} />}</For>
        <For each={diagram().nodes}>{point => {
          const node = point.data
          const duration = numeric(node, "*Duration (exclusive)")
          const badge = numeric(node, "*Planner Row Estimate Factor")
          return <foreignObject x={point.x - CARD_WIDTH / 2} y={point.y} width={CARD_WIDTH} height={height(point)}>
            <div data-plan-card class={`diagram-card ${props.selected === node.nodeId ? "selected" : ""}`} onMouseDown={e => e.stopPropagation()} onClick={() => props.onSelect(node)}>
              <div class="diagram-card-head"><button aria-label={isExpanded(node.nodeId) ? "Collapse node" : "Expand node"} title={isExpanded(node.nodeId) ? "Collapse node" : "Expand node"} class="card-expand" onClick={e => { e.stopPropagation(); toggle(node.nodeId) }}>{isExpanded(node.nodeId) ? "⌃" : "⌄"}</button><strong>{title(node)}</strong>
                <Show when={(numeric(node, "*Cost (exclusive)") || 0) > (props.plan.content.maxCost || Infinity) * .4}><span class="node-badge" title="High relative cost">$</span></Show>
                <Show when={badge !== undefined && badge >= 10}><span class="node-badge warn" title="Row estimate mismatch">!</span></Show>
                <span class="node-id">#{node.nodeId}</span>
              </div>
              <Show when={node["Relation Name"] || node["Index Name"]}><div class="diagram-relation"><span>{node["Relation Name"] ? "on" : "using"}</span> {String(node["Schema"] ? `${node["Schema"]}.` : "")}{String(node["Relation Name"] || node["Index Name"])}</div></Show>
              <Show when={node["Relation Name"] && node["Index Name"]}><div class="diagram-relation"><span>using</span> {String(node["Index Name"])}</div></Show>
              <div class="diagram-track"><div class={barPercent(node) >= 65 ? "hot" : barPercent(node) >= 30 ? "warm" : ""} style={{ width: `${barPercent(node)}%` }} /></div>
              <div class="diagram-value">{metric()}: <strong>{formatted(value(node), metric() === "duration" ? " ms" : "")}</strong><Show when={duration !== undefined && metric() !== "duration"}><span> · {formatted(duration, " ms")}</span></Show></div>
              <Show when={isExpanded(node.nodeId)}><div class="card-expanded"><span>Estimated rows <b>{formatted(numeric(node, "Plan Rows"))}</b></span><span>Actual rows <b>{formatted(numeric(node, "Actual Rows"))}</b></span><span>Loops <b>{formatted(numeric(node, "Actual Loops"))}</b></span></div></Show>
            </div>
          </foreignObject>
        }}</For>
      </g>
    </svg>
      <Show when={inspected()}>{node => <div class="canvas-inspector" onMouseDown={e => e.stopPropagation()}><div class="canvas-inspector-head"><strong>{title(node())} <small>#{node().nodeId}</small></strong><button aria-label="Close node details" onClick={() => props.onSelect(null)}>×</button></div>
        <div class="canvas-inspector-fields"><For each={Object.entries(node()).filter(([key, value]) => !key.startsWith("*") && !["Plans", "Workers", "nodeId", "size"].includes(key) && value !== null && typeof value !== "object")}>
          {([key, value]) => <div><span>{key}</span><strong>{String(value)}</strong></div>}
        </For></div></div>}</Show>
    </div>
  </div>
}
