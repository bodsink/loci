const LABEL_COLOR = {
  File: "#60a5fa",
  Package: "#38bdf8",
  Module: "#22d3ee",
  Function: "#5eead4",
  Method: "#2dd4bf",
  Class: "#a78bfa",
  Struct: "#c084fc",
  Interface: "#818cf8",
  Trait: "#e879f9",
  Type: "#d8b4fe",
  Enum: "#f0abfc",
  Field: "#94a3b8",
  Route: "#f472b6",
  Adr: "#fbbf24",
  TraceSpan: "#fb7185",
  Project: "#e2e8f0",
};

const EDGE_COLOR = {
  CALLS: "#5eead4",
  IMPORTS: "#60a5fa",
  ROUTES_TO: "#f472b6",
  INHERITS: "#c084fc",
  IMPLEMENTS: "#818cf8",
  CONTAINS: "#475569",
  DEFINES: "#64748b",
  HAS_FIELD: "#94a3b8",
  CALL_UNRESOLVED: "#fb7185",
  IMPACTS: "#fbbf24",
};

const state = {
  projects: [],
  tools: [],
  project: null,
  tab: "atlas",
  status: null,
  architecture: null,
  graph: null,
  selected: null,
  snippet: null,
  searchHits: [],
  trace: null,
  coverage: null,
  changes: null,
  usage: null,
  usageWindow: "day",
  surface: "performance",
  graphView: "calls",
  graphSeed: "",
};

const $ = (id) => document.getElementById(id);

async function apiTool(name, args = {}) {
  const res = await fetch(`/api/tools/${name}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(args),
  });
  const data = await res.json();
  if (data.error && res.status >= 400) throw Object.assign(new Error(data.message || data.error), data);
  return data;
}

async function apiGraph(project, params) {
  const q = new URLSearchParams(params);
  const res = await fetch(`/api/projects/${encodeURIComponent(project)}/graph?${q}`);
  return res.json();
}

function toast(message, kind = "ok") {
  const el = document.createElement("div");
  el.className = "toast";
  el.style.borderColor = kind === "err" ? "var(--rose)" : "rgba(94,234,212,0.4)";
  el.textContent = message;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), 4200);
}

function fmtTime(unix) {
  if (!unix) return "—";
  return new Date(unix * 1000).toLocaleString();
}

function esc(value) {
  return String(value ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

async function loadProjects() {
  const data = await apiTool("list_projects", { limit: 200 });
  state.projects = data.projects || [];
  renderProjects();
  if (state.surface === "performance") {
    return;
  }
  if (!state.project && state.projects.length) {
    await selectProject(state.projects[0].project);
  } else if (state.project) {
    const still = state.projects.find((p) => p.project === state.project);
    if (!still) {
      state.project = null;
      await selectProject(state.projects[0]?.project);
    }
  } else {
    renderWorkspace();
  }
}

function setSurface(name) {
  state.surface = name;
  const perf = $("nav-performance");
  if (perf) perf.classList.toggle("active", name === "performance");
  $("tabs").hidden = name === "performance";
  const search = $("global-search");
  if (search && search.parentElement) search.parentElement.hidden = name === "performance";
}

function renderProjects() {
  $("projects").innerHTML = state.projects
    .map(
      (p) => `
      <div class="project-row ${state.surface === "project" && p.project === state.project ? "active" : ""}">
        <button class="project" type="button" data-id="${esc(p.project)}">
          <b>${esc(p.name || p.project)}</b>
          <small>${esc(p.root)}</small>
        </button>
        <button class="project-remove" type="button" data-delete="${esc(p.project)}" title="Remove ${esc(p.name || p.project)}">Remove</button>
      </div>`
    )
    .join("") || `<div class="empty">No projects yet.</div>`;
  $("projects").querySelectorAll(".project").forEach((btn) => {
    btn.onclick = () => selectProject(btn.dataset.id);
  });
  $("projects").querySelectorAll("[data-delete]").forEach((btn) => {
    btn.onclick = (ev) => {
      ev.stopPropagation();
      removeProject(btn.dataset.delete);
    };
  });
}

async function openPerformance() {
  setSurface("performance");
  state.tab = "usage";
  $("project-title").textContent = "Day performance";
  $("project-root").textContent = "MCP tool calls across every indexed project.";
  $("reindex").hidden = true;
  $("delete-project").hidden = true;
  renderProjects();
  $("workspace").innerHTML = `<div class="empty">Loading day performance…</div>`;
  await loadUsage();
  renderWorkspace();
}

async function selectProject(id) {
  if (!id) {
    await openPerformance();
    return;
  }
  setSurface("project");
  if (state.tab === "usage") state.tab = "atlas";
  document.querySelectorAll(".tabs button").forEach((b) =>
    b.classList.toggle("active", b.dataset.tab === state.tab)
  );
  state.project = id;
  state.selected = null;
  state.snippet = null;
  renderProjects();
  const entry = state.projects.find((p) => p.project === id);
  $("project-title").textContent = entry?.name || id;
  $("project-root").textContent = entry?.root || "";
  $("reindex").hidden = false;
  $("delete-project").hidden = false;
  $("workspace").innerHTML = `<div class="empty">Loading ${esc(id)}…</div>`;
  try {
    const [status, architecture] = await Promise.all([
      apiTool("index_status", { project: id }),
      apiTool("get_architecture", { project: id }),
    ]);
    state.status = status;
    state.architecture = architecture;
    if (state.tab === "atlas") await loadGraph();
    if (state.tab === "coverage") await loadCoverage();
    renderWorkspace();
  } catch (error) {
    $("workspace").innerHTML = `<div class="empty">${esc(error.message)}</div>`;
  }
}

async function loadGraph(seed = state.graphSeed) {
  if (!state.project) return;
  const payload = await apiGraph(state.project, {
    view: state.graphView,
    seed: seed || "",
    depth: "2",
    limit: "220",
  });
  if (payload.error) {
    toast(payload.message || payload.error, "err");
    return;
  }
  state.graph = payload;
  state.graphSeed = seed || "";
}

async function loadCoverage() {
  if (!state.project) return;
  const [coverage, changes] = await Promise.all([
    apiTool("check_index_coverage", { project: state.project, scopes: ["."] }),
    apiTool("detect_changes", { project: state.project, limit: 80 }),
  ]);
  state.coverage = coverage;
  state.changes = changes;
}

function renderWorkspace() {
  const root = $("workspace");
  if (state.tab === "usage") {
    renderUsage(root);
    return;
  }
  if (!state.project) {
    root.innerHTML = `<div class="empty">Add a project to open its atlas.</div>`;
    return;
  }
  if (state.tab === "atlas") renderAtlas(root);
  else if (state.tab === "overview") renderOverview(root);
  else if (state.tab === "explore") renderExplore(root);
  else if (state.tab === "tools") renderTools(root);
  else renderCoverage(root);
}

async function loadUsage() {
  const windowName = state.usageWindow === "all" ? "all" : "day";
  const res = await fetch(`/api/usage?window=${windowName}`);
  state.usage = await res.json();
}

function renderAtlas(root) {
  const graph = state.graph || { nodes: [], edges: [], note: "" };
  root.innerHTML = `
    <div class="atlas">
      <div class="canvas-wrap">
        <div class="atlas-toolbar">
          ${["overview", "imports", "calls", "routes", "neighborhood"]
            .map(
              (view) =>
                `<button type="button" data-view="${view}" class="${
                  state.graphView === view ? "active" : ""
                }">${view}</button>`
            )
            .join("")}
        </div>
        <canvas id="graph"></canvas>
        <div class="legend">
          ${Object.entries(EDGE_COLOR)
            .slice(0, 6)
            .map(([k, c]) => `<span><i style="background:${c}"></i>${k}</span>`)
            .join("")}
        </div>
        ${graph.note ? `<div class="note">${esc(graph.note)}</div>` : ""}
        <div class="atlas-tip" id="atlas-tip" hidden></div>
      </div>
    </div>`;
  root.querySelectorAll("[data-view]").forEach((btn) => {
    btn.onclick = async () => {
      state.graphView = btn.dataset.view;
      await loadGraph(state.graphSeed);
      renderWorkspace();
    };
  });
  const canvas = $("graph");
  if (canvas) startGraph(canvas, graph);
  if (state.selected) showAtlasTip(state.selected);
}

function atlasTipHtml(node) {
  const file = node.file_path
    ? `${node.file_path}${node.start_line ? `:${node.start_line}` : ""}`
    : "—";
  return `<strong>${esc(node.name || node.qualified_name)}</strong>
    <small>${esc(node.label || "Node")} · in ${node.in_degree ?? "—"} · out ${node.out_degree ?? "—"}</small>
    <code>${esc(node.qualified_name || node.name || "")}</code>
    <small>${esc(file)}</small>`;
}

function showAtlasTip(node) {
  const tip = $("atlas-tip");
  if (!tip) return;
  if (!node) {
    tip.hidden = true;
    tip.innerHTML = "";
    return;
  }
  tip.hidden = false;
  tip.innerHTML = atlasTipHtml(node);
}

function inspectorHtml() {
  const node = state.selected;
  if (!node) {
    return `<p class="empty">Pick a hit.</p>`;
  }
  return `
    <h2>${esc(node.name)}</h2>
    <div class="kv">
      <div><span>label</span><b>${esc(node.label)}</b></div>
      <div><span>qualified</span><b>${esc(node.qualified_name)}</b></div>
      <div><span>file</span><b>${esc(node.file_path)}:${node.start_line}-${node.end_line}</b></div>
      <div><span>language</span><b>${esc(node.language || "—")}</b></div>
      <div><span>degree</span><b>in ${node.in_degree} · out ${node.out_degree}</b></div>
    </div>
    ${state.snippet?.source ? `<pre class="snippet">${esc(state.snippet.source)}</pre>` : `<p class="empty">Loading source…</p>`}
    <div class="row" style="margin-top:10px;justify-content:flex-start">
      <button class="ghost" id="open-trace" type="button">Trace calls</button>
    </div>`;
}

function renderOverview(root) {
  const a = state.architecture || {};
  const s = state.status || {};
  const langs = Object.entries(a.languages || {});
  const labels = Object.entries(a.labels || {});
  const maxLang = Math.max(1, ...langs.map(([, n]) => n));
  const maxLabel = Math.max(1, ...labels.map(([, n]) => n));
  root.innerHTML = `
    <div class="cards">
      ${stat("Nodes", s.nodes)}
      ${stat("Edges", s.edges)}
      ${stat("Files", s.files)}
      ${stat("Indexed", s.coverage?.indexed)}
    </div>
    <div class="bars">
      <div class="card">
        <h3>Languages</h3>
        ${langs.map(([k, n]) => bar(k, n, maxLang)).join("") || empty()}
      </div>
      <div class="card">
        <h3>Labels</h3>
        ${labels.map(([k, n]) => bar(k, n, maxLabel)).join("") || empty()}
      </div>
    </div>
    <div class="split" style="margin-top:12px">
      <div class="card">
        <h3>HTTP routes</h3>
        <table class="table">
          <tr><th>method</th><th>path</th><th>handler</th></tr>
          ${(a.routes || [])
            .map(
              (r) =>
                `<tr><td>${esc(r.method)}</td><td>${esc(r.path)}</td><td>${esc(
                  r.handler?.qualified_name || "—"
                )}</td></tr>`
            )
            .join("")}
        </table>
      </div>
      <div class="card">
        <h3>Largest files</h3>
        <table class="table">
          <tr><th>file</th><th>symbols</th></tr>
          ${(a.largest_files_by_symbol_count || [])
            .map((f) => `<tr><td>${esc(f.file_path)}</td><td>${f.symbols}</td></tr>`)
            .join("")}
        </table>
      </div>
    </div>`;
}

function successRate(ok, calls) {
  if (!calls) return 0;
  return Math.round((ok / calls) * 100);
}

function windowToggle(active) {
  return `<div class="usage-window">
    <button type="button" class="chip${active === "day" ? " active" : ""}" data-window="day">Last 24 hours</button>
    <button type="button" class="chip${active === "all" ? " active" : ""}" data-window="all">All</button>
  </div>`;
}

function bindWindowToggle(root) {
  root.querySelectorAll("[data-window]").forEach((btn) => {
    btn.onclick = async () => {
      if (state.usageWindow === btn.dataset.window) return;
      state.usageWindow = btn.dataset.window;
      root.innerHTML = `<div class="empty">Loading…</div>`;
      await loadUsage();
      renderWorkspace();
    };
  });
}

function sessionRows(sessions) {
  if (!sessions.length) return "";
  const rows = sessions
    .map((s) => {
      const projects = (s.projects || []).join(", ") || "—";
      const first = s.list_projects_first ? s.first_tool : `${s.first_tool} (not list_projects)`;
      return `<tr><td>${fmtTime(s.first_at_unix)} → ${fmtTime(s.last_at_unix)}</td><td>${s.calls}</td><td>${esc(first)}</td><td>${esc(projects)}</td></tr>`;
    })
    .join("");
  return `<h3 class="usage-heading">Sessions</h3>
    <div class="card">
      <table class="table">
        <tr><th>when</th><th>calls</th><th>started with</th><th>projects</th></tr>
        ${rows}
      </table>
    </div>`;
}

function usageBucket(u, projectId) {
  return (u.by_project || []).find((p) => p.project === projectId) || null;
}

function stat(label, value, tone) {
  return `<div class="stat${tone ? ` tone-${tone}` : ""}"><em>${label}</em><strong>${value ?? "—"}</strong></div>`;
}
function bar(label, value, max) {
  return `<div class="bar"><span>${esc(label)}</span><i><b style="width:${Math.round((value / max) * 100)}%"></b></i><span>${value}</span></div>`;
}
function empty() {
  return `<div class="empty">Nothing counted.</div>`;
}

function renderExplore(root) {
  root.innerHTML = `
    <div class="explore">
      <div class="card" style="overflow:auto">
        <h3>search_graph</h3>
        <div class="fields">
          <div class="field"><label>name or pattern</label><input id="ex-q" value=""></div>
          <div class="field"><label>label</label>
            <select id="ex-label">
              <option value="">any</option>
              ${Object.keys(LABEL_COLOR).map((l) => `<option>${l}</option>`).join("")}
            </select>
          </div>
          <button class="add" id="ex-go" type="button">Search</button>
        </div>
        <div class="list" id="ex-hits"></div>
      </div>
      <div class="card" style="overflow:auto">
        <h3>trace + snippet</h3>
        <div id="ex-detail">${state.selected ? inspectorHtml() : `<p class="empty">Pick a hit.</p>`}</div>
        ${state.trace ? renderTrace(state.trace) : ""}
      </div>
    </div>`;
  $("ex-go").onclick = runExploreSearch;
  renderHits();
  const traceBtn = $("open-trace");
  if (traceBtn) traceBtn.onclick = () => runTrace(state.selected?.qualified_name);
}

function renderHits() {
  const box = $("ex-hits");
  if (!box) return;
  box.innerHTML = (state.searchHits || [])
    .map(
      (h) => `<button class="hit" data-qn="${esc(h.qualified_name)}">
        <b>${esc(h.name)}</b>
        <small>${esc(h.label)} · ${esc(h.file_path)}:${h.start_line} · in ${h.in_degree} out ${h.out_degree}</small>
      </button>`
    )
    .join("") || `<p class="empty">No hits.</p>`;
  box.querySelectorAll(".hit").forEach((btn) => {
    btn.onclick = () => openSymbol(btn.dataset.qn);
  });
}

function renderTrace(trace) {
  const row = (title, items) =>
    `<h3>${title}</h3>` +
    (items || [])
      .map(
        (c) =>
          `<div class="hit"><b>${esc(c.qualified_name)}</b><small>hop ${c.hop} · ${esc(c.edge_type)}</small></div>`
      )
      .join("") || `<p class="empty">None.</p>`;
  return `${row("Callers", trace.callers)}${row("Callees", trace.callees)}
    ${(trace.unresolved || []).length ? `<p class="empty">${trace.unresolved.length} unresolved calls</p>` : ""}`;
}

async function runExploreSearch() {
  const name = $("ex-q").value.trim();
  const label = $("ex-label").value;
  const args = { project: state.project, limit: 50 };
  if (name.includes("*") || name.startsWith("^")) args.name_pattern = name;
  else if (name) args.name = name;
  if (label) args.label = label;
  if (!name && !label) {
    toast("Pass a name or a label", "err");
    return;
  }
  const data = await apiTool("search_graph", args);
  state.searchHits = data.results || [];
  renderHits();
}

async function openSymbol(qn) {
  const [search, snippet] = await Promise.all([
    apiTool("search_graph", { project: state.project, qualified_name: qn, limit: 1 }),
    apiTool("get_code_snippet", { project: state.project, qualified_name: qn, context_lines: 2 }),
  ]);
  state.selected = (search.results || [])[0] || { qualified_name: qn, name: qn.split(".").pop() };
  state.snippet = snippet;
  if (state.tab === "explore") renderWorkspace();
}

async function runTrace(qn) {
  if (!qn) return;
  state.trace = await apiTool("trace_path", {
    project: state.project,
    qualified_name: qn,
    direction: "both",
    depth: 3,
  });
  state.tab = "explore";
  document.querySelectorAll(".tabs button").forEach((b) => b.classList.toggle("active", b.dataset.tab === "explore"));
  renderWorkspace();
}

function renderTools(root) {
  root.innerHTML = `<div class="tools">${state.tools
    .map(
      (t) => `<article class="tool-card">
        <h3>${esc(t.name)}</h3>
        <p>${esc(t.description)}</p>
        <button class="ghost" data-tool="${esc(t.name)}" type="button">Run</button>
      </article>`
    )
    .join("")}</div>`;
  root.querySelectorAll("[data-tool]").forEach((btn) => {
    btn.onclick = () => openTool(btn.dataset.tool);
  });
}

function openTool(name) {
  const tool = state.tools.find((t) => t.name === name);
  if (!tool) return;
  const props = tool.inputSchema?.properties || {};
  const fields = Object.entries(props)
    .map(([key, schema]) => {
      const required = (tool.inputSchema.required || []).includes(key);
      const value = key === "project" ? state.project || "" : "";
      if (schema.type === "boolean") {
        return `<label class="check"><input type="checkbox" data-k="${esc(key)}"> ${esc(key)}${required ? " *" : ""}</label>`;
      }
      if (schema.enum) {
        return `<div class="field"><label>${esc(key)}${required ? " *" : ""}</label>
          <select data-k="${esc(key)}"><option value=""></option>${schema.enum
            .map((v) => `<option ${v === value ? "selected" : ""}>${esc(v)}</option>`)
            .join("")}</select></div>`;
      }
      if (schema.type === "object" || schema.type === "array") {
        return `<div class="field"><label>${esc(key)} (JSON)</label><textarea data-k="${esc(key)}" data-json="1" rows="4">${
          key === "start" ? '{"label":"Route"}' : ""
        }</textarea></div>`;
      }
      return `<div class="field"><label>${esc(key)}${required ? " *" : ""}</label>
        <input data-k="${esc(key)}" value="${esc(value)}" placeholder="${esc(schema.description || "")}"></div>`;
    })
    .join("");
  showModal(`
    <div class="sheet">
      <h2>${esc(name)}</h2>
      <p class="empty">${esc(tool.description)}</p>
      <div class="fields">${fields}</div>
      <div class="row">
        <button class="ghost" id="modal-cancel" type="button">Close</button>
        <button class="add" id="tool-run" type="button">Call tool</button>
      </div>
      <pre class="snippet" id="tool-out" hidden></pre>
    </div>`);
  $("modal-cancel").onclick = hideModal;
  $("tool-run").onclick = async () => {
    const args = {};
    $("modal").querySelectorAll("[data-k]").forEach((el) => {
      const key = el.dataset.k;
      if (el.type === "checkbox") {
        if (el.checked) args[key] = true;
        return;
      }
      const raw = el.value.trim();
      if (!raw) return;
      if (el.dataset.json) {
        try {
          args[key] = JSON.parse(raw);
        } catch {
          toast(`Invalid JSON for ${key}`, "err");
        }
      } else if (el.type === "number") args[key] = Number(raw);
      else args[key] = raw;
    });
    try {
      const result = await apiTool(name, args);
      const out = $("tool-out");
      out.hidden = false;
      out.textContent = JSON.stringify(result, null, 2);
      if (name === "list_projects" || name === "index_repository" || name === "delete_project") {
        await loadProjects();
      }
    } catch (error) {
      toast(error.message, "err");
    }
  };
}

function renderUsage(root) {
  const u = state.usage;
  if (!u) {
    root.innerHTML = `<div class="empty">Loading agent usage…</div>`;
    return;
  }
  if (!u.total_calls) {
    const emptyDay = u.window === "day";
    root.innerHTML = `<div class="usage">${windowToggle(state.usageWindow)}
      <div class="empty">${
        emptyDay
          ? "No MCP tool calls in the last 24 hours. Switch to All to see the rest of the journal."
          : `No MCP tool calls recorded yet. The journal is ${esc(u.journal_path || "agent_calls.jsonl")}.`
      }</div>
    </div>`;
    bindWindowToggle(root);
    return;
  }
  if (state.surface === "project" && state.project) {
    renderProjectUsage(root, u, state.project);
    return;
  }
  const criteria = u.criteria || {};
  const toolStats = Object.entries(u.by_tool_stats || {});
  const maxTool = Math.max(1, ...toolStats.map(([, s]) => s.calls));
  const selected = state.project;
  const window =
    u.first_at_unix && u.last_at_unix
      ? `${fmtTime(u.first_at_unix)} → ${fmtTime(u.last_at_unix)}`
      : "";
  const success = successRate(u.ok, u.total_calls);
  const pageRate =
    criteria.pagination_needed > 0
      ? `${criteria.pagination_followed}/${criteria.pagination_needed}`
      : "—";
  const walkRate =
    criteria.walk_calls > 0
      ? `${criteria.walk_calls - criteria.walk_failures}/${criteria.walk_calls}`
      : "—";

  root.innerHTML = `
    <div class="usage">
      ${windowToggle(u.window || state.usageWindow)}
      <div class="card">
        <h3>Global · ${u.window === "day" ? "last 24 hours" : "all journal"} · ${esc(window)}</h3>
        <p class="empty" style="padding:0 0 10px">Source: ${esc(u.journal_path)}. The questions are from docs/cursor-agent.md.</p>
        <div class="cards">
          ${stat("Calls", u.total_calls)}
          ${stat("Succeeded", `${success}%`, success >= 80 ? "ok" : "warn")}
          ${stat("Graph / grep", `${u.graph_calls} / ${u.text_search_calls}`, u.graph_calls >= u.text_search_calls ? "ok" : "warn")}
          ${stat("Walk tools ok", walkRate, criteria.walk_failures ? "bad" : "ok")}
        </div>
        <table class="table" style="margin-top:12px">
          <tr><th>Design question</th><th>Observed</th></tr>
          <tr><td>list_projects first?</td><td>${criteria.list_projects_first || 0} / ${criteria.sessions || 0} sessions</td></tr>
          <tr><td>Does it page?</td><td>${pageRate} has_more followed by cursor</td></tr>
          <tr><td>search_graph over search_code?</td><td>${u.graph_calls} structural · ${u.text_search_calls} search_code</td></tr>
          <tr><td>Walk tools (trace_path, query_graph)</td><td>${criteria.walk_failures || 0} failed of ${criteria.walk_calls || 0}</td></tr>
          <tr><td>Freshness after edits?</td><td>detect_changes ${criteria.detect_changes || 0} · check_index_coverage ${criteria.check_index_coverage || 0}</td></tr>
          <tr><td>Wrong project id?</td><td class="${criteria.project_not_found ? "tone-bad" : ""}">${criteria.project_not_found || 0} project_not_found</td></tr>
        </table>
      </div>
      ${sessionRows(u.sessions || [])}
      <div class="bars">
        <div class="card">
          <h3>Tools</h3>
          ${toolStats
            .sort((a, b) => b[1].calls - a[1].calls)
            .map(
              ([name, s]) =>
                `${bar(name, s.calls, maxTool)}<small class="usage-meta">${s.fail} fail · p50 ${s.duration_p50_ms} ms · p95 ${s.duration_p95_ms} ms</small>`
            )
            .join("") || empty()}
        </div>
        <div class="card">
          <h3>Errors</h3>
          ${
            Object.keys(u.by_error_code || {}).length
              ? `<table class="table"><tr><th>code</th><th>count</th></tr>${Object.entries(
                  u.by_error_code
                )
                  .map(([code, n]) => `<tr><td>${esc(code)}</td><td>${n}</td></tr>`)
                  .join("")}</table>`
              : `<p class="empty">No recorded errors.</p>`
          }
          ${(u.common_sequences || []).length
            ? `<h3 style="margin-top:16px">Common sequences</h3><table class="table">${u.common_sequences
                .map(([seq, n]) => `<tr><td>${esc(seq)}</td><td>${n}</td></tr>`)
                .join("")}</table>`
            : ""}
        </div>
      </div>
      <h3 class="usage-heading">Per project</h3>
      <div class="usage-projects">
        ${(u.by_project || [])
          .map((p) => {
            const id = p.project || "(no project)";
            const active = p.project && p.project === selected;
            const tools = Object.entries(p.by_tool || {})
              .sort((a, b) => b[1] - a[1])
              .map(([name, n]) => `<tr><td>${esc(name)}</td><td>${n}</td></tr>`)
              .join("");
            const errors = Object.entries(p.by_error_code || {})
              .map(([code, n]) => `${esc(code)} ${n}`)
              .join(" · ");
            const rate = successRate(p.ok, p.calls);
            return `<div class="card${active ? " active" : ""}">
              <h3>${esc(id)}${active ? " · selected" : ""}</h3>
              <div class="cards cards-3">
                ${stat("Calls", p.calls)}
                ${stat("Succeeded", `${rate}%`, rate >= 80 ? "ok" : "warn")}
                ${stat("Failed", p.fail, p.fail ? "bad" : "ok")}
              </div>
              <p class="usage-meta">${fmtTime(p.first_at_unix)} → ${fmtTime(p.last_at_unix)} · paged ${p.paged_followthrough}/${p.truncated_responses}${
                errors ? ` · ${errors}` : ""
              }</p>
              <table class="table"><tr><th>tool</th><th>calls</th></tr>${tools}</table>
            </div>`;
          })
          .join("")}
      </div>
    </div>`;
  bindWindowToggle(root);
}

function renderProjectUsage(root, u, projectId) {
  const p = usageBucket(u, projectId);
  if (!p) {
    root.innerHTML = `<div class="usage">${windowToggle(state.usageWindow)}
      <div class="empty">No MCP tool calls recorded for ${esc(
        projectId
      )} in this window. Calls without a project argument are only on Day performance.</div>
    </div>`;
    bindWindowToggle(root);
    return;
  }
  const rate = successRate(p.ok, p.calls);
  const tools = Object.entries(p.by_tool || {})
    .sort((a, b) => b[1] - a[1])
    .map(([name, n]) => `<tr><td>${esc(name)}</td><td>${n}</td></tr>`)
    .join("");
  const errors = Object.entries(p.by_error_code || {})
    .map(([code, n]) => `<tr><td>${esc(code)}</td><td>${n}</td></tr>`)
    .join("");
  const sessions = (u.sessions || []).filter((s) => (s.projects || []).includes(projectId));
  root.innerHTML = `
    <div class="usage">
      ${windowToggle(u.window || state.usageWindow)}
      <div class="card">
        <h3>${esc(p.project)} · ${u.window === "day" ? "last 24 hours" : "all journal"} · ${fmtTime(p.first_at_unix)} → ${fmtTime(p.last_at_unix)}</h3>
        <div class="cards">
          ${stat("Calls", p.calls)}
          ${stat("Succeeded", `${rate}%`, rate >= 80 ? "ok" : "warn")}
          ${stat("Failed", p.fail, p.fail ? "bad" : "ok")}
          ${stat("Graph / grep", `${p.graph_calls} / ${p.text_search_calls}`)}
        </div>
        <p class="usage-meta">paged ${p.paged_followthrough}/${p.truncated_responses} · from ${esc(
          u.journal_path
        )}</p>
      </div>
      ${sessionRows(sessions)}
      <div class="bars">
        <div class="card">
          <h3>Tools</h3>
          ${
            tools
              ? `<table class="table"><tr><th>tool</th><th>calls</th></tr>${tools}</table>`
              : empty()
          }
        </div>
        <div class="card">
          <h3>Errors</h3>
          ${
            errors
              ? `<table class="table"><tr><th>code</th><th>count</th></tr>${errors}</table>`
              : `<p class="empty">No recorded errors.</p>`
          }
        </div>
      </div>
    </div>`;
  bindWindowToggle(root);
}

function renderCoverage(root) {
  const c = state.coverage || {};
  const ch = state.changes || {};
  const files = c.scopes?.[0]?.files || c.files || [];
  root.innerHTML = `
    <div class="cards">
      ${stat("Indexed", state.status?.coverage?.indexed)}
      ${stat("Partial", state.status?.coverage?.parse_partial)}
      ${stat("Skipped", state.status?.coverage?.skipped)}
      ${stat("Changed", (ch.added || 0) + (ch.modified || 0) + (ch.removed || 0))}
    </div>
    <div class="split">
      <div class="card">
        <h3>detect_changes</h3>
        ${(ch.files || [])
          .map((f) => `<div class="hit"><b>${esc(f.path)}</b><small>${esc(f.change)}</small></div>`)
          .join("") || `<p class="empty">Working tree matches the index.</p>`}
      </div>
      <div class="card">
        <h3>coverage sample</h3>
        ${files
          .slice(0, 40)
          .map(
            (f) =>
              `<div class="hit"><b>${esc(f.path || f.file_path || "")}</b><small>${esc(
                f.status || ""
              )} ${esc(f.reason || "")}</small></div>`
          )
          .join("") || `<p class="empty">${esc(c.note || state.status?.coverage?.note || "")}</p>`}
      </div>
    </div>`;
}

function showModal(html) {
  $("modal").hidden = false;
  $("modal").innerHTML = html;
}
function hideModal() {
  $("modal").hidden = true;
  $("modal").innerHTML = "";
}

function openAddProject() {
  showModal(`
    <div class="sheet">
      <h2>Add project</h2>
      <p class="empty">Absolute path to a repository root. Loci reads only under that path.</p>
      <div class="fields">
        <div class="field"><label>repo_path</label><input id="ap-path" placeholder="/home/you/src/app"></div>
        <div class="field"><label>name (optional)</label><input id="ap-name" placeholder="defaults to the directory name"></div>
        <label class="check"><input id="ap-full" type="checkbox"> Full rebuild</label>
        <label class="check"><input id="ap-lsp" type="checkbox"> Hybrid LSP</label>
      </div>
      <div class="row">
        <button class="ghost" id="modal-cancel" type="button">Cancel</button>
        <button class="add" id="ap-go" type="button">Index</button>
      </div>
    </div>`);
  $("modal-cancel").onclick = hideModal;
  $("ap-go").onclick = indexFromModal;
}

async function indexFromModal() {
  const repo_path = $("ap-path").value.trim();
  if (!repo_path.startsWith("/")) {
    toast("repo_path must be absolute", "err");
    return;
  }
  const args = { repo_path, full: $("ap-full").checked, hybrid_lsp: $("ap-lsp").checked };
  const name = $("ap-name").value.trim();
  if (name) args.name = name;
  $("ap-go").disabled = true;
  $("ap-go").textContent = "Indexing…";
  try {
    const report = await apiTool("index_repository", args);
    hideModal();
    toast(`Indexed ${report.project} · ${report.nodes} nodes`);
    await loadProjects();
    await selectProject(report.project);
  } catch (error) {
    toast(error.message, "err");
    $("ap-go").disabled = false;
    $("ap-go").textContent = "Index";
  }
}

function startGraph(canvas, payload) {
  const nodes = (payload.nodes || []).map((n, i) => ({
    ...n,
    x: Math.cos((i / Math.max(1, payload.nodes.length)) * Math.PI * 2) * 180,
    y: Math.sin((i / Math.max(1, payload.nodes.length)) * Math.PI * 2) * 180,
    vx: 0,
    vy: 0,
  }));
  const idMap = new Map(nodes.map((n) => [n.id, n]));
  const edges = (payload.edges || [])
    .map((e) => ({ ...e, a: idMap.get(e.src), b: idMap.get(e.dst) }))
    .filter((e) => e.a && e.b);

  const ctx = canvas.getContext("2d");
  const wrap = canvas.parentElement;
  let w = 0;
  let h = 0;
  let scale = 1;
  let panX = 0;
  let panY = 0;
  let drag = null;
  let panning = null;
  let hover = null;
  const particles = edges.filter((e) => e.edge_type === "CALLS" || e.edge_type === "ROUTES_TO").map((e) => ({
    e,
    t: Math.random(),
  }));

  function resize() {
    const dpr = window.devicePixelRatio || 1;
    w = wrap.clientWidth;
    h = wrap.clientHeight;
    canvas.width = w * dpr;
    canvas.height = h * dpr;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  resize();
  window.addEventListener("resize", resize);

  function toWorld(sx, sy) {
    return { x: (sx - w / 2 - panX) / scale, y: (sy - h / 2 - panY) / scale };
  }
  function nodeAt(sx, sy) {
    const p = toWorld(sx, sy);
    return nodes.find((n) => (n.x - p.x) ** 2 + (n.y - p.y) ** 2 < (radius(n) + 4) ** 2);
  }
  function radius(n) {
    return 5 + Math.min(10, Math.sqrt((n.in_degree || 0) + (n.out_degree || 0)));
  }

  canvas.onwheel = (ev) => {
    ev.preventDefault();
    scale = Math.min(3.2, Math.max(0.25, scale * (ev.deltaY < 0 ? 1.08 : 0.92)));
  };
  canvas.onmousedown = (ev) => {
    const n = nodeAt(ev.offsetX, ev.offsetY);
    if (n) drag = { n, dx: 0 };
    else panning = { x: ev.offsetX - panX, y: ev.offsetY - panY };
  };
  canvas.onmousemove = (ev) => {
    hover = nodeAt(ev.offsetX, ev.offsetY);
    canvas.style.cursor = hover || drag ? "pointer" : panning ? "grabbing" : "crosshair";
    if (drag) {
      const p = toWorld(ev.offsetX, ev.offsetY);
      drag.n.x = p.x;
      drag.n.y = p.y;
      drag.n.vx = 0;
      drag.n.vy = 0;
    } else if (panning) {
      panX = ev.offsetX - panning.x;
      panY = ev.offsetY - panning.y;
    }
  };
  canvas.onmouseup = (ev) => {
    const n = nodeAt(ev.offsetX, ev.offsetY);
    if (n && drag) {
      state.selected = n;
      showAtlasTip(n);
    } else if (!n && !drag) {
      state.selected = null;
      showAtlasTip(null);
    }
    drag = null;
    panning = null;
  };
  canvas.ondblclick = async (ev) => {
    const n = nodeAt(ev.offsetX, ev.offsetY);
    if (!n) return;
    state.graphView = "neighborhood";
    await loadGraph(n.qualified_name);
    renderWorkspace();
  };

  function tick() {
    const k = Math.sqrt((w * h) / Math.max(nodes.length, 1)) * 0.22;
    for (let i = 0; i < nodes.length; i += 1) {
      for (let j = i + 1; j < nodes.length; j += 1) {
        let dx = nodes[i].x - nodes[j].x;
        let dy = nodes[i].y - nodes[j].y;
        let dist = Math.hypot(dx, dy) || 0.01;
        const force = (k * k) / dist;
        dx = (dx / dist) * force;
        dy = (dy / dist) * force;
        nodes[i].vx += dx;
        nodes[i].vy += dy;
        nodes[j].vx -= dx;
        nodes[j].vy -= dy;
      }
    }
    for (const e of edges) {
      let dx = e.b.x - e.a.x;
      let dy = e.b.y - e.a.y;
      const dist = Math.hypot(dx, dy) || 0.01;
      const force = (dist - k * 2.1) * 0.012;
      dx = (dx / dist) * force;
      dy = (dy / dist) * force;
      e.a.vx += dx;
      e.a.vy += dy;
      e.b.vx -= dx;
      e.b.vy -= dy;
    }
    for (const n of nodes) {
      n.vx += -n.x * 0.002;
      n.vy += -n.y * 0.002;
      n.vx *= 0.86;
      n.vy *= 0.86;
      if (drag && drag.n === n) continue;
      n.x += n.vx;
      n.y += n.vy;
    }
    draw();
    requestAnimationFrame(tick);
  }

  function draw() {
    ctx.clearRect(0, 0, w, h);
    ctx.save();
    ctx.translate(w / 2 + panX, h / 2 + panY);
    ctx.scale(scale, scale);
    for (const e of edges) {
      const color = EDGE_COLOR[e.edge_type] || "#64748b";
      ctx.beginPath();
      ctx.strokeStyle = color;
      ctx.globalAlpha = 0.35;
      ctx.lineWidth = e.edge_type === "CALLS" ? 1.6 : 1;
      const mx = (e.a.x + e.b.x) / 2 + (e.a.y - e.b.y) * 0.12;
      const my = (e.a.y + e.b.y) / 2 + (e.b.x - e.a.x) * 0.12;
      ctx.moveTo(e.a.x, e.a.y);
      ctx.quadraticCurveTo(mx, my, e.b.x, e.b.y);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
    for (const p of particles) {
      p.t = (p.t + 0.006) % 1;
      const mx = (p.e.a.x + p.e.b.x) / 2 + (p.e.a.y - p.e.b.y) * 0.12;
      const my = (p.e.a.y + p.e.b.y) / 2 + (p.e.b.x - p.e.a.x) * 0.12;
      const t = p.t;
      const x = (1 - t) * (1 - t) * p.e.a.x + 2 * (1 - t) * t * mx + t * t * p.e.b.x;
      const y = (1 - t) * (1 - t) * p.e.a.y + 2 * (1 - t) * t * my + t * t * p.e.b.y;
      ctx.fillStyle = EDGE_COLOR[p.e.edge_type] || "#fff";
      ctx.beginPath();
      ctx.arc(x, y, 2.2, 0, Math.PI * 2);
      ctx.fill();
    }
    for (const n of nodes) {
      const color = LABEL_COLOR[n.label] || "#e2e8f0";
      const r = radius(n);
      ctx.shadowColor = color;
      ctx.shadowBlur = n === hover || n === state.selected ? 22 : 10;
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.arc(n.x, n.y, r, 0, Math.PI * 2);
      ctx.fill();
      if (n.label === "Route") {
        ctx.strokeStyle = "#fff";
        ctx.lineWidth = 1.2;
        ctx.beginPath();
        ctx.arc(n.x, n.y, r + 3, 0, Math.PI * 2);
        ctx.stroke();
      }
      ctx.shadowBlur = 0;
      if (scale > 0.7 || n === hover || n === state.selected) {
        ctx.fillStyle = "rgba(232,234,244,0.9)";
        ctx.font = "11px ui-sans-serif";
        ctx.fillText(n.name, n.x + r + 4, n.y + 3);
      }
    }
    ctx.restore();
  }
  tick();
}

$("nav-performance").onclick = () => openPerformance();
$("add-project").onclick = openAddProject;
$("reindex").onclick = async () => {
  const entry = state.projects.find((p) => p.project === state.project);
  if (!entry) return;
  toast("Reindexing…");
  try {
    const report = await apiTool("index_repository", { repo_path: entry.root });
    toast(`Indexed ${report.nodes} nodes`);
    await selectProject(state.project);
  } catch (error) {
    toast(error.message, "err");
  }
};
async function removeProject(id) {
  if (!id) return;
  const entry = state.projects.find((p) => p.project === id);
  const label = entry?.name || id;
  if (!confirm(`Remove ${label} from Loci? The source repository is not touched.`)) return;
  try {
    await apiTool("delete_project", { project: id });
    if (state.project === id) {
      state.project = null;
      state.status = null;
      state.architecture = null;
      state.graph = null;
    }
    toast(`Removed ${label}`);
    await loadProjects();
  } catch (error) {
    toast(error.message, "err");
  }
}

$("delete-project").onclick = () => removeProject(state.project);
$("tabs").onclick = async (ev) => {
  const btn = ev.target.closest("[data-tab]");
  if (!btn) return;
  state.tab = btn.dataset.tab;
  document.querySelectorAll(".tabs button").forEach((b) => b.classList.toggle("active", b === btn));
  if (state.tab === "atlas" && !state.graph) await loadGraph();
  if (state.tab === "coverage" && !state.coverage) await loadCoverage();
  if (state.tab === "usage") await loadUsage();
  renderWorkspace();
};
$("global-search").onkeydown = async (ev) => {
  if (ev.key !== "Enter" || !state.project) return;
  const q = ev.target.value.trim();
  if (!q) return;
  state.tab = "explore";
  document.querySelectorAll(".tabs button").forEach((b) => b.classList.toggle("active", b.dataset.tab === "explore"));
  renderWorkspace();
  $("ex-q").value = q;
  await runExploreSearch();
};
$("modal").onclick = (ev) => {
  if (ev.target.id === "modal") hideModal();
};

(async function boot() {
  try {
    const tools = await (await fetch("/api/tools")).json();
    state.tools = tools.tools || [];
  } catch {
    state.tools = [];
  }
  await loadProjects();
  await openPerformance();
})();
