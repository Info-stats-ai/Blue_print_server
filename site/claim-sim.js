(function () {
  // The claims demo: a local simulation, nothing is sent anywhere.
  var FUNCTIONS = ["complete_task", "add_task", "delete_task"];
  var AGENTS = { alice: "Alice's agent", bob: "Bob's agent" };
  document.querySelectorAll("[data-sim]").forEach(function (root) {
    var state = { agent: "alice", mode: "function", held: {} };
    state.held = { complete_task: "bob" };
    var message = "You're Alice's agent. Bob's agent holds complete_task. Click a function to claim it.";
    var tone = "";

    function add(parent, tag, className, text) {
      var node = document.createElement(tag);
      if (className) node.className = className;
      if (text) node.textContent = text;
      parent.appendChild(node);
      return node;
    }
    function fileOwner() {
      var names = Object.keys(state.held);
      return names.length ? state.held[names[0]] : null;
    }
    function click(name) {
      var me = AGENTS[state.agent];
      var holder = state.held[name];
      if (holder === state.agent) {
        delete state.held[name];
        message = me + " released " + name + ".";
        tone = "";
      } else if (holder) {
        message = "Refused: " + AGENTS[holder] + " is working on " + name + ".";
        tone = "refused";
      } else if (state.mode === "file" && fileOwner() && fileOwner() !== state.agent) {
        message = "Refused: " + AGENTS[fileOwner()] + " has locked the whole file, so " + name + " has to wait.";
        tone = "refused";
      } else {
        state.held[name] = state.agent;
        message = "Granted: " + me + " now holds " + name + ".";
        tone = "granted";
      }
      render();
    }
    function render() {
      root.textContent = "";
      var bar = add(root, "div", "sim-bar");
      add(bar, "span", "sim-label", "Act as");
      Object.keys(AGENTS).forEach(function (id) {
        var button = add(bar, "button", "sim-chip " + id, AGENTS[id]);
        button.type = "button";
        button.setAttribute("aria-pressed", String(state.agent === id));
        button.addEventListener("click", function () {
          state.agent = id;
          message = "You're " + AGENTS[id] + ". Click a function to claim it.";
          tone = "";
          render();
        });
      });
      var compare = add(bar, "label", "sim-toggle");
      var toggle = add(compare, "input");
      toggle.type = "checkbox";
      toggle.checked = state.mode === "file";
      add(compare, "span", "", "Compare with file locks");
      toggle.addEventListener("change", function () {
        state.mode = toggle.checked ? "file" : "function";
        message = toggle.checked ? "File locks: one agent holds the whole file. Try claiming something." : "Function claims: agents only hold the function they're changing.";
        tone = "";
        render();
      });
      var file = add(root, "div", "sim-file");
      add(file, "div", "tab", "tasks.py · example");
      FUNCTIONS.forEach(function (name) {
        var holder = state.held[name];
        var locked = state.mode === "file" && !holder && fileOwner();
        var row = add(file, "button", "sim-fn" + (holder ? " held-" + holder : "") + (locked ? " locked" : ""));
        row.type = "button";
        add(row, "code", "", "def " + name + "(…)");
        add(row, "span", "sim-owner", holder ? AGENTS[holder] : locked ? "Locked by " + AGENTS[fileOwner()] : "Free");
        row.addEventListener("click", function () { click(name); });
      });
      var status = add(root, "p", "sim-status " + tone, message);
      status.setAttribute("role", "status");
    }
    render();
  });

})();
