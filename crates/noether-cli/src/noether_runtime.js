/**
 * NoetherRuntime — the reactive UI runtime for Noether browser apps.
 *
 * Usage:
 *   const runtime = new NoetherRuntime(executeFn, mountEl, executeStageFn, getGraphJsonFn);
 *   runtime.defineAtom('count', 0);
 *   runtime.defineEvent('increment', (atoms) => ({ count: atoms.count + 1 }));
 *   await runtime.render();
 *
 * When `executeStageFn` and `getGraphJsonFn` are provided (browser builds with
 * RemoteStage support), the JS graph executor drives execution: local Stage
 * nodes are dispatched via WASM `execute_stage()` and RemoteStage nodes are
 * called via `fetch()`. Otherwise, the monolithic WASM `execute()` is used.
 *
 * VNode format (produced by Rust stages):
 *   { tag: "div", props: { class: "foo", onClick: { $event: "name" } }, children: [...] }
 *   { $text: "some text" }  — text node
 *   null / undefined        — renders nothing
 *
 * Event protocol:
 *   { $event: "name" }
 *     → calls the registered handler with current atoms, merges returned patch.
 *   { $event: "set", $target: "atomName", $attr: "value" }
 *     → sets atoms[atomName] = event.target[attr]  (for inputs, selects, etc.)
 *   { $event: "toggle", $target: "atomName" }
 *     → toggles a boolean atom
 */
class NoetherRuntime {
  /**
   * @param {function(string): string}   executeFn      — legacy full-graph wasm-bindgen execute()
   * @param {HTMLElement}                mountEl        — where to render the app
   * @param {function(string,string): string} [executeStageFn] — wasm-bindgen execute_stage(id, json)
   * @param {function(): string}         [getGraphJsonFn]      — wasm-bindgen get_graph_json()
   */
  constructor(executeFn, mountEl, executeStageFn, getGraphJsonFn) {
    this._execute = executeFn;
    this._mount = mountEl;
    this._executeStage = executeStageFn || null;
    this._getGraphJson = getGraphJsonFn || null;
    this._atoms = {};           // name → current value
    this._events = {};          // name → handler function(atoms) → partial atoms patch
    this._vdom = null;          // last rendered VNode tree
    this._rendering = false;    // guard against re-entrant renders
    this._pendingRender = false;
    this._graphNode = null;     // parsed root CompositionNode, cached after first load

    // ── Routing ──────────────────────────────────────────────────────────────
    // Listen for browser back/forward and update _route atom automatically.
    if (typeof window !== 'undefined') {
      window.addEventListener('popstate', () => {
        this._atoms['_route'] = window.location.pathname;
        this._scheduleRender();
      });
    }
  }

  // ── Atom management ────────────────────────────────────────────────────────

  /**
   * Declare a reactive state atom with an initial value.
   * Atoms declared here are serialised as the composition input on each render.
   */
  defineAtom(name, initial) {
    this._atoms[name] = initial;
    return this;
  }

  /**
   * Set an atom value and schedule a re-render.
   * @param {string} name
   * @param {*} value  — new value, or a function(current) → new value
   */
  setAtom(name, value) {
    const next = typeof value === 'function' ? value(this._atoms[name]) : value;
    this._atoms[name] = next;
    this._scheduleRender();
  }

  /**
   * Register an event handler.
   * @param {string} name      — event name referenced in VNode props
   * @param {function} handler — (atoms: Record) → Partial<Record>  patch applied to atoms
   */
  defineEvent(name, handler) {
    this._events[name] = handler;
    return this;
  }

  // ── Routing ────────────────────────────────────────────────────────────────

  /**
   * Navigate to a new path using the History API.
   *
   * Sets `_route` atom to the new path and schedules a re-render.
   * The `noether.router` stdlib stage reads `_route` to select the active view.
   *
   * @param {string} path  — e.g. "/todos", "/settings/profile"
   * @param {*}      [state] — optional History state object
   */
  navigate(path, state) {
    if (typeof window !== 'undefined') {
      window.history.pushState(state || null, '', path);
    }
    this._atoms['_route'] = path;
    this._scheduleRender();
  }

  // ── Render loop ────────────────────────────────────────────────────────────

  _scheduleRender() {
    if (this._rendering) {
      this._pendingRender = true;
      return;
    }
    // Use microtask to batch synchronous atom mutations.
    Promise.resolve().then(() => this.render());
  }

  /**
   * Execute the composition graph with current atom state, then diff+patch the DOM.
   *
   * When the JS graph executor is available (executeStageFn + getGraphJsonFn),
   * it drives execution: local stages go to WASM, RemoteStage nodes go to fetch().
   * Otherwise falls back to the monolithic WASM execute().
   */
  async render() {
    if (this._rendering) {
      this._pendingRender = true;
      return;
    }
    this._rendering = true;
    this._pendingRender = false;

    try {
      const input = { ...this._atoms };
      // Auto-inject current route path so noether.router stages can consume it
      // without requiring the user to declare a _route atom manually.
      if (typeof window !== 'undefined' && !('_route' in input)) {
        input['_route'] = window.location.pathname;
      }
      let output;

      if (this._executeStage && this._getGraphJson) {
        // JS graph executor path — supports RemoteStage
        if (!this._graphNode) {
          const graphJson = this._getGraphJson();
          const graph = JSON.parse(graphJson);
          this._graphNode = graph.root;
        }
        output = await this._executeGraph(this._graphNode, input);
      } else {
        // Legacy path — monolithic WASM execute()
        const resultJson = this._execute(JSON.stringify(input));
        const result = JSON.parse(resultJson);
        if (!result.ok) {
          this._renderError(result.error || 'Unknown execution error');
          return;
        }
        output = result.output;
      }

      this._patch(this._mount, output, this._vdom);
      this._vdom = output;
    } catch (err) {
      this._renderError(String(err));
    } finally {
      this._rendering = false;
      if (this._pendingRender) {
        this._pendingRender = false;
        // Schedule the pending render as a microtask to avoid stack overflow.
        Promise.resolve().then(() => this.render());
      }
    }
  }

  // ── JS Graph Executor ──────────────────────────────────────────────────────

  /**
   * Execute a composition node recursively, returning the output value.
   * Handles all CompositionNode variants — local stages via WASM,
   * RemoteStage nodes via fetch().
   *
   * @param {object} node   — CompositionNode (from graph JSON)
   * @param {*}      input  — JSON-serialisable input value
   * @returns {Promise<*>}  — output value
   */
  async _executeGraph(node, input) {
    const op = node.op;

    switch (op) {
      case 'Stage':
        return this._execLocal(node.id, input);

      case 'RemoteStage':
        return this._execRemote(node.url, input);

      case 'Const':
        return node.value;

      case 'Sequential': {
        let current = input;
        for (const stage of node.stages) {
          current = await this._executeGraph(stage, current);
        }
        return current;
      }

      case 'Parallel': {
        // Execute all branches concurrently; each branch may get its sub-field of input.
        const entries = Object.entries(node.branches);
        const results = await Promise.all(
          entries.map(([name, branch]) => {
            const branchInput =
              input && typeof input === 'object' && name in input
                ? input[name]
                : input;
            return this._executeGraph(branch, branchInput).then(out => [name, out]);
          })
        );
        const merged = {};
        for (const [name, out] of results) {
          merged[name] = out;
        }
        return merged;
      }

      case 'Branch': {
        const pred = await this._executeGraph(node.predicate, input);
        const branch = pred ? node.if_true : node.if_false;
        return this._executeGraph(branch, input);
      }

      case 'Fanout': {
        const sourceOut = await this._executeGraph(node.source, input);
        const results = await Promise.all(
          node.targets.map(t => this._executeGraph(t, sourceOut))
        );
        return results;
      }

      case 'Merge': {
        // Gather outputs from all sources, then pass merged record to target.
        const sourceResults = await Promise.all(
          node.sources.map((s, i) => {
            const srcInput =
              input && typeof input === 'object' && (`source_${i}`) in input
                ? input[`source_${i}`]
                : input;
            return this._executeGraph(s, srcInput).then(out => [`source_${i}`, out]);
          })
        );
        const merged = {};
        for (const [k, v] of sourceResults) {
          merged[k] = v;
        }
        return this._executeGraph(node.target, merged);
      }

      case 'Retry': {
        const maxAttempts = node.max_attempts || 3;
        let lastErr;
        for (let i = 0; i < maxAttempts; i++) {
          try {
            return await this._executeGraph(node.stage, input);
          } catch (e) {
            lastErr = e;
            if (node.delay_ms) {
              await new Promise(r => setTimeout(r, node.delay_ms));
            }
          }
        }
        throw lastErr || new Error(`Retry exhausted after ${maxAttempts} attempts`);
      }

      default:
        throw new Error(`Unknown CompositionNode op: ${op}`);
    }
  }

  /**
   * Execute a single local stage via WASM `execute_stage(id, inputJson)`.
   * @param {string} stageId
   * @param {*} input
   * @returns {Promise<*>}
   */
  async _execLocal(stageId, input) {
    const resultJson = this._executeStage(stageId, JSON.stringify(input));
    const result = JSON.parse(resultJson);
    if (!result.ok) {
      throw new Error(`Stage ${stageId} failed: ${result.error}`);
    }
    return result.output;
  }

  /**
   * Execute a RemoteStage via HTTP POST.
   *
   * Sends `{"input": value}` to `url` and extracts `data.output` from the
   * ACLI response envelope `{"data": {"output": ...}}`.
   *
   * @param {string} url
   * @param {*} input
   * @returns {Promise<*>}
   */
  async _execRemote(url, input) {
    const resp = await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ input }),
    });
    if (!resp.ok) {
      throw new Error(`RemoteStage call to ${url} returned HTTP ${resp.status}`);
    }
    const json = await resp.json();
    if (!json.data || !('output' in json.data)) {
      throw new Error(`RemoteStage response from ${url} missing data.output field`);
    }
    return json.data.output;
  }

  _renderError(msg) {
    this._mount.innerHTML =
      `<div class="nr-error"><strong>Noether runtime error</strong><pre>${_escHtml(msg)}</pre></div>`;
  }

  // ── VNode diffing + DOM patching ───────────────────────────────────────────

  /**
   * Recursively patch `parent`'s content to match `newVnode`.
   * `oldVnode` is the previously rendered tree (or null on first render).
   *
   * Strategy:
   *  - Text nodes are patched by content.
   *  - Element nodes are patched by tag + key (if present).
   *  - Children are reconciled by index (keyed reconciliation is a future enhancement).
   */
  _patch(parent, newVnode, oldVnode) {
    // ── Removal ─────────────────────────────────────────────────────────────
    if (newVnode == null) {
      if (oldVnode != null) {
        _clearChildren(parent);
      }
      return;
    }

    // ── First render (no old vdom) ────────────────────────────────────────────
    if (oldVnode == null) {
      parent.appendChild(this._createEl(newVnode));
      return;
    }

    // ── Both are text nodes ───────────────────────────────────────────────────
    if (newVnode.$text != null && oldVnode.$text != null) {
      const child = parent.firstChild;
      if (child && child.nodeType === Node.TEXT_NODE) {
        if (child.textContent !== newVnode.$text) {
          child.textContent = newVnode.$text;
        }
      } else {
        _clearChildren(parent);
        parent.appendChild(document.createTextNode(newVnode.$text));
      }
      return;
    }

    // ── Type changed (text ↔ element, or different tag) ───────────────────────
    const oldTag = oldVnode.$text != null ? '#text' : (oldVnode.tag || '');
    const newTag = newVnode.$text != null ? '#text' : (newVnode.tag || '');

    if (oldTag !== newTag) {
      _clearChildren(parent);
      parent.appendChild(this._createEl(newVnode));
      return;
    }

    // ── Same element tag: patch in place ──────────────────────────────────────
    const el = parent.firstElementChild || parent.firstChild;
    if (!el) {
      parent.appendChild(this._createEl(newVnode));
      return;
    }

    // Update props/attrs
    this._patchProps(el, newVnode.props || {}, oldVnode.props || {});

    // Reconcile children
    const newChildren = newVnode.children || [];
    const oldChildren = oldVnode.children || [];
    this._patchChildren(el, newChildren, oldChildren);
  }

  /**
   * Reconcile the children of `el` from `oldChildren` to `newChildren`.
   *
   * Keyed strategy (when children carry `props.key`):
   *   1. Index old DOM children by key using `__nrKey` tags.
   *   2. For each new child: reuse + patch the matching old DOM node, or create fresh.
   *   3. Append reused/new nodes in new order; remove orphaned old nodes.
   *
   * Keyless children fall back to the original index-based reconciliation.
   * Mixed keyed/keyless lists use index-based reconciliation (all-or-nothing per list).
   */
  _patchChildren(el, newChildren, oldChildren) {
    // Decide strategy: if ANY new child has a key, use the keyed path.
    const hasKeys = newChildren.some(c => c && c.props && c.props.key != null);

    if (hasKeys) {
      this._patchChildrenKeyed(el, newChildren, oldChildren);
    } else {
      this._patchChildrenIndexed(el, newChildren, oldChildren);
    }
  }

  /**
   * Keyed reconciliation: reuse DOM nodes by stable `props.key`.
   */
  _patchChildrenKeyed(el, newChildren, oldChildren) {
    // Build a map from key → { domNode, oldVnode } for currently rendered children.
    const oldByKey = new Map();
    const domChildren = Array.from(el.childNodes);

    for (let i = 0; i < oldChildren.length; i++) {
      const oldVnode = oldChildren[i];
      const domNode = domChildren[i];
      if (!oldVnode || !domNode) continue;
      const key = oldVnode.props && oldVnode.props.key;
      if (key != null) {
        oldByKey.set(key, { domNode, oldVnode });
        domNode.__nrKey = key;
      }
    }

    // Build the new child list in DOM order.
    const newDomNodes = [];

    for (const newVnode of newChildren) {
      if (newVnode == null) continue;
      const key = newVnode.props && newVnode.props.key;

      if (key != null && oldByKey.has(key)) {
        const { domNode, oldVnode } = oldByKey.get(key);
        oldByKey.delete(key); // mark as consumed

        // Patch props and children in place.
        if (newVnode.$text != null) {
          if (domNode.nodeType === Node.TEXT_NODE) {
            if (domNode.textContent !== newVnode.$text) {
              domNode.textContent = newVnode.$text;
            }
          }
        } else {
          this._patchProps(domNode, newVnode.props || {}, oldVnode.props || {});
          this._patchChildren(domNode, newVnode.children || [], oldVnode.children || []);
        }
        newDomNodes.push(domNode);
      } else {
        // No existing node for this key — create fresh.
        newDomNodes.push(this._createEl(newVnode));
      }
    }

    // Remove any old nodes whose keys were not consumed.
    for (const { domNode } of oldByKey.values()) {
      if (domNode.parentNode === el) {
        el.removeChild(domNode);
      }
    }

    // Reorder/append to match newChildren order.
    for (let i = 0; i < newDomNodes.length; i++) {
      const node = newDomNodes[i];
      const current = el.childNodes[i];
      if (current !== node) {
        el.insertBefore(node, current || null);
      }
    }

    // Remove any trailing DOM nodes beyond the new list length.
    while (el.childNodes.length > newDomNodes.length) {
      el.removeChild(el.lastChild);
    }
  }

  /**
   * Index-based reconciliation (original algorithm, used when no keys are present).
   */
  _patchChildrenIndexed(el, newChildren, oldChildren) {
    const maxLen = Math.max(newChildren.length, oldChildren.length);
    for (let i = 0; i < maxLen; i++) {
      const newChild = newChildren[i];
      const oldChild = oldChildren[i];

      if (newChild == null && oldChild != null) {
        // Remove extra child.
        const domChild = _getNthChild(el, i);
        if (domChild) el.removeChild(domChild);
        continue;
      }

      if (newChild != null && oldChild == null) {
        // Append new child.
        el.appendChild(this._createEl(newChild));
        continue;
      }

      // Plain string/number node
      if (typeof newChild === 'string' || typeof newChild === 'number') {
        const txt = String(newChild);
        const domChild = _getNthChild(el, i);
        if (domChild && domChild.nodeType === Node.TEXT_NODE) {
          if (domChild.textContent !== txt) domChild.textContent = txt;
        } else if (domChild) {
          el.replaceChild(document.createTextNode(txt), domChild);
        } else {
          el.appendChild(document.createTextNode(txt));
        }
        continue;
      }

      // Both exist: patch in a temporary wrapper pointing at child i.
      const domChild = _getNthChild(el, i);
      if (!domChild) {
        el.appendChild(this._createEl(newChild));
        continue;
      }

      // Build a temporary wrapper so _patch can work with a parent.
      // Check if tags match; if not, replace.
      const oldTag = oldChild.$text != null ? '#text' : (oldChild.tag || '');
      const newTag = newChild.$text != null ? '#text' : (newChild.tag || '');

      if (oldTag !== newTag) {
        el.replaceChild(this._createEl(newChild), domChild);
      } else if (newChild.$text != null) {
        if (domChild.nodeType === Node.TEXT_NODE) {
          if (domChild.textContent !== newChild.$text) {
            domChild.textContent = newChild.$text;
          }
        } else {
          el.replaceChild(document.createTextNode(newChild.$text), domChild);
        }
      } else {
        this._patchProps(domChild, newChild.props || {}, oldChild.props || {});
        this._patchChildren(domChild, newChild.children || [], oldChild.children || []);
      }
    }
  }

  /**
   * Patch DOM element props/attributes from old to new.
   */
  _patchProps(el, newProps, oldProps) {
    // Remove old props that are gone.
    for (const key of Object.keys(oldProps)) {
      if (key.startsWith('$')) continue;
      if (!(key in newProps)) {
        _removeProp(el, key);
      }
    }
    // Set new/changed props.
    for (const [key, val] of Object.entries(newProps)) {
      if (key.startsWith('$')) continue;
      if (typeof val === 'object' && val !== null && val.$event) {
        // Bind event handler.
        _bindEvent(el, key, val, this);
      } else if (oldProps[key] !== val) {
        _setProp(el, key, val);
      }
    }
  }

  /**
   * Create a DOM node from a VNode (recursively).
   */
  _createEl(vnode) {
    if (vnode == null) return document.createTextNode('');
    if (typeof vnode === 'string' || typeof vnode === 'number') return document.createTextNode(String(vnode));
    if (vnode.$text != null) return document.createTextNode(vnode.$text);

    const el = document.createElement(vnode.tag || 'div');

    for (const [key, val] of Object.entries(vnode.props || {})) {
      if (key.startsWith('$')) continue;
      if (typeof val === 'object' && val !== null && val.$event) {
        _bindEvent(el, key, val, this);
      } else {
        _setProp(el, key, val);
      }
    }

    for (const child of (vnode.children || [])) {
      el.appendChild(this._createEl(child));
    }
    return el;
  }

  // ── Event dispatch ─────────────────────────────────────────────────────────

  /**
   * Dispatch an event from the UI.
   * Called by bound DOM event listeners.
   *
   * @param {object} eventSpec  — the $event object from the VNode prop
   * @param {Event}  domEvent   — the native DOM event
   */
  dispatchEvent(eventSpec, domEvent) {
    const name = eventSpec.$event;

    // Built-in "set" event: sets atom[target] = event.target[attr]
    if (name === 'set' && eventSpec.$target) {
      const attr = eventSpec.$attr || 'value';
      const value = domEvent.target ? domEvent.target[attr] : eventSpec.$value;
      this.setAtom(eventSpec.$target, value);
      return;
    }

    // Built-in "toggle" event: flips a boolean atom
    if (name === 'toggle' && eventSpec.$target) {
      this.setAtom(eventSpec.$target, v => !v);
      return;
    }

    // Built-in "set-value" event: sets atom to a literal value
    if (name === 'set-value' && eventSpec.$target) {
      this.setAtom(eventSpec.$target, eventSpec.$value);
      return;
    }

    // Built-in "navigate" event: client-side route transition
    if (name === 'navigate' && eventSpec.$path) {
      this.navigate(eventSpec.$path);
      return;
    }

    // Registered handler
    const handler = this._events[name];
    if (handler) {
      const patch = handler({ ...this._atoms }, domEvent, eventSpec);
      if (patch && typeof patch === 'object') {
        Object.assign(this._atoms, patch);
        this._scheduleRender();
      }
      return;
    }

    console.warn(`NoetherRuntime: no handler for event "${name}"`);
  }
}

// ── DOM helpers ───────────────────────────────────────────────────────────────

function _setProp(el, key, val) {
  if (key === 'class') {
    el.className = val;
  } else if (key === 'style' && typeof val === 'object') {
    Object.assign(el.style, val);
  } else if (key === 'style' && typeof val === 'string') {
    el.style.cssText = val;
  } else if (key in el && key !== 'list' && key !== 'form') {
    // Direct DOM property (value, checked, disabled, etc.)
    try { el[key] = val; } catch (_) { el.setAttribute(key, val); }
  } else {
    el.setAttribute(key, val);
  }
}

function _removeProp(el, key) {
  if (key === 'class') {
    el.className = '';
  } else if (key in el) {
    try { el[key] = null; } catch (_) { el.removeAttribute(key); }
  } else {
    el.removeAttribute(key);
  }
}

/**
 * Bind a DOM event listener from a VNode event spec.
 * We use a data attribute to detect stale listeners and avoid duplicate binds.
 */
function _bindEvent(el, propKey, eventSpec, runtime) {
  // Map VNode prop names to DOM event names.
  const domEvent = _propToEvent(propKey);
  if (!domEvent) return;

  const specKey = `__nr_${domEvent}`;
  const specJson = JSON.stringify(eventSpec);

  // Remove old listener if event spec changed.
  if (el[specKey + '_spec'] !== specJson) {
    if (el[specKey]) {
      el.removeEventListener(domEvent, el[specKey]);
    }
    const listener = (e) => {
      e.preventDefault && e.preventDefault();
      runtime.dispatchEvent(eventSpec, e);
    };
    el[specKey] = listener;
    el[specKey + '_spec'] = specJson;
    el.addEventListener(domEvent, listener);
  }
}

function _propToEvent(propKey) {
  // Accept both lowercase html names (onclick, oninput) and React-style camelCase (onClick, onInput).
  const norm = propKey.toLowerCase();
  if (!norm.startsWith('on')) return null;
  return norm.slice(2) || null;
}

function _clearChildren(el) {
  while (el.firstChild) el.removeChild(el.firstChild);
}

function _getNthChild(el, n) {
  return el.childNodes[n] || null;
}

function _escHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}
