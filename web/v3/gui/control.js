/**
 * Capabilities-Driven Radar Control Panel
 *
 * Dynamically builds the control UI based on radar capabilities.
 * Control building and updating logic adapted from v1/gui/control.js.
 * WebSocket streaming uses Signal K v3 protocol.
 */

export {
  loadRadar,
  registerRadarCallback,
  registerControlCallback,
  setCurrentRange,
  getPowerState,
  getControl,
  getOperatingTime,
  isPlaybackMode,
  getUserName,
  togglePower,
  zoomIn,
  zoomOut,
  getCurrentRangeDisplay,
};

import van from "./van-1.5.2.js";
import { toUser } from "./units.js";
import {
  fetchRadars,
  fetchRadarIds,
  fetchCapabilities,
  setControl as apiSetControl,
  detectMode,
  isStandaloneMode,
  saveInstallationSetting,
  isPlaybackRadar,
} from "./api.js";

const { div, label, input, button, select, option, span } = van.tags;

// State
let radarId = null;
let myr_capabilities = null;
let stateWebSocket = null;
let radarCallbacks = [];
let controlCallbacks = [];
let playbackMode = false;

// Control state (from v1)
let myr_control_values = {};
let myr_error_message = null;

// Current range (for viewer.js integration)
let currentRange = 1852;
let lastRangeUpdateTime = 0;
let rangeUpdateCount = {};
let rangeFromSpokeData = false;

const control_prefix = "myr_control_";
const auto_postfix = "_auto";
const end_postfix = "_end";
const enabled_postfix = "_enabled";

function registerRadarCallback(callback) {
  radarCallbacks.push(callback);
}

function registerControlCallback(callback) {
  controlCallbacks.push(callback);
}

// Called from viewer.js when spoke data contains range
function setCurrentRange(meters) {
  if (meters <= 0) return;

  const now = Date.now();
  if (now - lastRangeUpdateTime > 2000) {
    rangeUpdateCount = {};
  }
  lastRangeUpdateTime = now;
  rangeUpdateCount[meters] = (rangeUpdateCount[meters] || 0) + 1;

  let maxCount = 0;
  let dominantRange = currentRange;
  for (const [range, count] of Object.entries(rangeUpdateCount)) {
    if (count > maxCount) {
      maxCount = count;
      dominantRange = parseInt(range);
    }
  }

  if (maxCount >= 5 && dominantRange !== currentRange) {
    currentRange = dominantRange;
    rangeFromSpokeData = true;
    const ranges = myr_capabilities?.supportedRanges || [];
    const newIndex = ranges.findIndex((r) => Math.abs(r - dominantRange) < 50);
    if (newIndex >= 0) {
      userRequestedRangeIndex = newIndex;
    }
    rangeUpdateCount = {};
  }
}

// ============================================================================
// Helper Classes (from v1)
// ============================================================================

class TemporaryMessage {
  timeoutId;
  element;

  constructor(id) {
    this.element = document.getElementById(id);
    this.element.style.hidden = true;
  }

  raise(aMessage) {
    this.element.style.hidden = false;
    this.element.classList.remove("myr_vanish");
    this.element.innerHTML = aMessage;
    this.timeoutId = setTimeout(() => {
      this.cancel();
    }, 5000);
  }

  cancel() {
    if (typeof this.timeoutId === "number") {
      clearTimeout(this.timeoutId);
    }
    this.element.classList.add("myr_vanish");
  }
}

// ============================================================================
// Control Building (from v1, adapted for v3 CSS)
// ============================================================================

function convertControlsToUserUnits(controls) {
  const result = {};

  Object.entries(controls).forEach(([id, control]) => {
    result[id] = convertControlToUserUnits(id, control);
  });

  return result;
}

function convertControlToUserUnits(id, control) {
  const result = {};

  let cloned = { id, ...control };

  if (cloned.units) {
    let units = cloned.units;
    if (units === "m" && cloned.maxValue < 100) {
      // leave this in meters
    } else {
      ["minValue", "maxValue", "stepValue"].forEach((prop) => {
        if (prop in cloned) {
          [units, cloned[prop]] = toUser(cloned.units, cloned[prop]);
        }
      });
    }
    cloned.user_units = units;
  }

  return cloned;
}

/**
 * Rounds a number to a limited number of decimals, for user pleasure.
 */
function roundToStep(value, stepValue) {
  value = Number(value);
  if (!Number.isFinite(value) || !Number.isFinite(stepValue)) return NaN;

  if (Math.abs(stepValue - 0.1) < Number.EPSILON) {
    return Number((value + stepValue / 2).toFixed(1));
  }
  if (stepValue < 0.02) {
    return Number((value + stepValue / 2).toFixed(2));
  }
  if (stepValue <= 1) {
    return Number((value + stepValue / 2).toFixed(1));
  }

  const scale = 1 / stepValue;
  const scaledVal = Math.round(value * scale);
  const scaledStep = Math.round(stepValue * scale);
  const roundedInt = Math.round(scaledVal / scaledStep) * scaledStep;
  const rounded = roundedInt / scale;

  return rounded;
}

// V1-style control builders adapted with v3 CSS classes
const ReadOnlyValue = (id, name) =>
  div(
    { class: "myr_control myr_readonly myr_info_stacked" },
    div({ class: "myr_control_label" }, name),
    div({ class: "myr_info_value", id: control_prefix + id })
  );

const StringValue = (id, name) =>
  div(
    { class: "myr_control myr_string_control" },
    span({ class: "myr_control_label" }, name),
    input({ type: "text", id: control_prefix + id, size: 20 }),
    button({ type: "button", onclick: (e) => do_button(e) }, "Set")
  );

const NumericValue = (id, name) =>
  div(
    { class: "myr_control myr_number_control" },
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      span({
        class: "myr_control_value myr_numeric",
        id: control_prefix + id + "_display",
      })
    ),
    input({
      type: "number",
      id: control_prefix + id,
      onchange: (e) => do_change(e.target),
      oninput: (e) => do_input(e),
    })
  );

const RangeValue = (id, name, min, max, def) =>
  div(
    { class: "myr_control myr_number_control" },
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      span({
        class: "myr_control_value myr_description",
        id: control_prefix + id + "_desc",
      })
    ),
    input({
      type: "range",
      class: "myr_slider",
      id: control_prefix + id,
      min,
      max,
      value: def,
      onchange: (e) => do_change(e.target),
    })
  );

// Discrete slider with tick marks showing possible values
const DiscreteSliderValue = (id, name, min, max, def) => {
  const numSteps = max - min;
  const ticks = [];
  for (let i = 0; i <= numSteps; i++) {
    ticks.push(
      div({
        class: "myr_tick",
        "data-index": i,
      })
    );
  }

  return div(
    { class: "myr_control myr_number_control" },
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      span({
        class: "myr_control_value myr_description",
        id: control_prefix + id + "_desc",
      })
    ),
    div(
      { class: "myr_discrete_slider", id: control_prefix + id + "_container" },
      div({ class: "myr_slider_track" }),
      div({ class: "myr_tick_container" }, ...ticks),
      input({
        type: "range",
        class: "myr_slider myr_slider_discrete",
        id: control_prefix + id,
        min,
        max,
        value: def,
        onchange: (e) => do_change(e.target),
        oninput: (e) => updateTickMarks(e.target),
      })
    )
  );
};

function updateTickMarks(slider) {
  const container = slider.closest(".myr_discrete_slider");
  if (!container) return;

  const min = parseInt(slider.min);
  const max = parseInt(slider.max);
  const value = parseInt(slider.value);

  const ticks = container.querySelectorAll(".myr_tick");
  ticks.forEach((tick, i) => {
    const tickValue = min + i;
    tick.classList.toggle("myr_tick_active", tickValue === value);
  });
}

const ButtonValue = (id, name) =>
  div(
    { class: "myr_control myr_button_control" },
    button(
      {
        type: "button",
        class: "myr_action_button",
        id: control_prefix + id,
        onclick: (e) => do_change(e.target),
      },
      name
    )
  );

const AutoButton = (id) =>
  button(
    {
      type: "button",
      class: "myr_auto_toggle",
      id: control_prefix + id + auto_postfix,
      onclick: (e) => do_toggle_auto(e.target),
    },
    "Auto"
  );

const EnabledButton = (id) =>
  div(
    { class: "myr_enabled_button" },
    label(
      {
        for: control_prefix + id + enabled_postfix,
        class: "myr_enabled_label",
      },
      "Enabled"
    ),
    input({
      type: "checkbox",
      class: "myr_enabled",
      id: control_prefix + id + enabled_postfix,
      onchange: (e) => do_change_enabled(e.target),
    })
  );

const SelectValue = (id, name, validValues, descriptions) => {
  return div(
    { class: "myr_control myr_enum_control" },
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      span({
        class: "myr_control_value",
        id: control_prefix + id + "_desc",
      })
    ),
    select(
      {
        class: "myr_select",
        id: control_prefix + id,
        onchange: (e) => do_change(e.target),
      },
      validValues.map((v) => option({ value: v }, descriptions[v]))
    )
  );
};

/**
 * Sector control - displays start and end angles with optional enabled checkbox
 * Server sends: value (start in radians), endValue (end in radians), enabled (optional)
 */
const SectorValue = (id, name, control) => {
  const prefix = `myr_control_${id}`;
  const hasEnabled = control.hasEnabled !== false;

  function sendSectorValue() {
    const startEl = document.getElementById(`${prefix}`);
    const endEl = document.getElementById(`${prefix}${end_postfix}`);
    const enabledEl = document.getElementById(`${prefix}${enabled_postfix}`);

    const startDeg = parseInt(startEl?.value) || 0;
    const endDeg = parseInt(endEl?.value) || 0;
    const enabledVal = enabledEl?.checked ?? true;

    // Convert degrees to radians for server
    const startRad = (startDeg * Math.PI) / 180;
    const endRad = (endDeg * Math.PI) / 180;

    apiSetControl(radarId, id, {
      value: startRad,
      endValue: endRad,
      enabled: enabledVal,
    });
  }

  function onEnabledChange(e) {
    const enabled = e.target.checked;
    const startEl = document.getElementById(`${prefix}_start`);
    const endEl = document.getElementById(`${prefix}_end`);
    if (startEl) startEl.disabled = !enabled;
    if (endEl) endEl.disabled = !enabled;
    sendSectorValue();
  }

  const min = control.minValue ?? -180;
  const max = control.maxValue ?? 180;

  return div(
    { class: "myr_control myr_sector_control", id: `myr_${id}` },
    span({ class: "myr_control_label" }, name),
    div(
      { class: "myr_sector_angles" },
      div(
        { class: "myr_sector_angle" },
        label({ for: `${control_prefix}${id}` }, "Start°"),
        input({
          type: "number",
          id: `${control_prefix}${id}`,
          min: min,
          max: max,
          value: 0,
          disabled: hasEnabled,
          onchange: () => sendSectorValue(),
        })
      ),
      div(
        { class: "myr_sector_angle" },
        label({ for: `${control_prefix}${id}${end_postfix}` }, "End°"),
        input({
          type: "number",
          id: `${control_prefix}${id}${end_postfix}`,
          min: min,
          max: max,
          value: 0,
          disabled: hasEnabled,
          onchange: () => sendSectorValue(),
        })
      )
    ),
    hasEnabled
      ? div(
          { class: "myr_sector_enabled" },
          label(
            { class: "myr_checkbox_label" },
            input({
              type: "checkbox",
              id: `${control_prefix}${id}${enabled_postfix}`,
              checked: false,
              onchange: onEnabledChange,
            }),
            " Enabled"
          )
        )
      : null
  );
};

/**
 * Update sector control UI from server state
 */
function updateSectorUI(id, control, cv) {
  const prefix = `myr_control_${id}`;
  const startEl = document.getElementById(`${prefix}`);
  const endEl = document.getElementById(`${prefix}${end_postfix}`);
  const enabledEl = document.getElementById(`${prefix}${enabled_postfix}`);

  const enabled = cv.enabled ?? false;

  let units, value, endValue;

  if (startEl) {
    [units, value] = toUser(control.units, cv.value);
    startEl.value = value;
    startEl.disabled = !enabled;
  }
  if (endEl) {
    [units, endValue] = toUser(control.units, cv.endValue);
    endEl.value = endValue;
    endEl.disabled = !enabled;
  }
  if (enabledEl) {
    enabledEl.checked = enabled;
  }
}

function buildControls() {
  let titleEl = get_element_by_server_id("myr_title");
  if (titleEl) {
    titleEl.innerHTML = "";
    const titleText = `${myr_capabilities.make || ""} ${
      myr_capabilities.model || ""
    } Controls`;
    if (playbackMode) {
      van.add(
        titleEl,
        div(
          { class: "myr_title_with_badge" },
          span(titleText),
          span({ class: "myr_playback_badge" }, "PLAYBACK")
        )
      );
    } else {
      van.add(titleEl, div(titleText));
    }
  }

  let controlsEl = document.getElementById("myr_controls");
  if (!controlsEl) return;
  controlsEl.innerHTML = "";

  // Group controls by category
  const baseControls = [];
  const advancedControls = [];
  const configControls = [];
  const infoControls = [];

  for (const [k, v] of Object.entries(myr_capabilities.controls)) {
    // Skip power control - it's handled by the power lozenge on the viewer
    if (k === "power") continue;
    // Skip range control - it's handled by the range lozenge on the viewer
    if (k === "range") continue;

    const control = { ...v, controlId: k };
    if (control.isReadOnly || control.readOnly) {
      infoControls.push(control);
    } else if (control.category === "installation") {
      configControls.push(control);
    } else if (control.category === "advanced") {
      advancedControls.push(control);
    } else {
      baseControls.push(control);
    }
  }

  // Sort by id
  const sortById = (a, b) => (a.id || 0) - (b.id || 0);
  baseControls.sort(sortById);
  advancedControls.sort(sortById);
  configControls.sort(sortById);
  infoControls.sort(sortById);

  // Build base controls section
  if (baseControls.length > 0) {
    const baseSection = div({ class: "myr_control_section" });
    van.add(controlsEl, baseSection);

    for (const control of baseControls) {
      const k = control.controlId;
      const v = control;

      van.add(baseSection, buildSingleControl(k, v));

      // Add auto/enabled buttons
      if (v.hasAuto) {
        van.add(get_element_by_server_id(k).parentNode, AutoButton(k));
      }
      if (v.hasEnabled && !v.isReadOnly) {
        van.add(get_element_by_server_id(k).parentNode, EnabledButton(k));
      }
    }
  }

  // Build advanced controls section
  if (advancedControls.length > 0) {
    const advancedSection = div(
      { class: "myr_control_section myr_advanced_section" },
      div({ class: "myr_section_header" }, "Advanced Controls")
    );
    van.add(controlsEl, advancedSection);

    for (const control of advancedControls) {
      const k = control.controlId;
      const v = control;
      van.add(advancedSection, buildSingleControl(k, v));
      if (v.hasAuto) {
        van.add(get_element_by_server_id(k).parentNode, AutoButton(k));
      }
      if (v.hasEnabled && !v.isReadOnly) {
        van.add(get_element_by_server_id(k).parentNode, EnabledButton(k));
      }
    }
  }

  // Build installation controls section
  if (configControls.length > 0) {
    const configSection = div(
      { class: "myr_control_section myr_installation_section" },
      div({ class: "myr_section_header" }, "Installation")
    );
    van.add(controlsEl, configSection);

    for (const control of configControls) {
      const k = control.controlId;
      const v = control;
      van.add(configSection, buildSingleControl(k, v));

      if (v.hasAuto) {
        van.add(get_element_by_server_id(k).parentNode, AutoButton(k));
      }
      if (v.hasEnabled && !v.isReadOnly) {
        van.add(get_element_by_server_id(k).parentNode, EnabledButton(k));
      }
    }
  }

  // Build info controls section
  if (infoControls.length > 0) {
    const infoSection = div(
      { class: "myr_control_section myr_info_section" },
      div({ class: "myr_section_header" }, "Radar Information")
    );
    van.add(controlsEl, infoSection);

    for (const control of infoControls) {
      const k = control.controlId;
      if (k == 0 && playbackMode) {
        van.add(
          infoSection,
          div({ class: "myr_control myr_error" }, "REPLAY MODE")
        );
      }
      van.add(infoSection, ReadOnlyValue(k, control.name));
    }
  }
}

function buildSingleControl(k, v) {
  if (v.isReadOnly || v.readOnly) {
    return ReadOnlyValue(k, v.name);
  } else if (v.dataType === "button") {
    return ButtonValue(k, v.name);
  } else if (v.dataType === "string") {
    return StringValue(k, v.name);
  } else if (v.dataType === "sector") {
    return SectorValue(k, v.name, v);
  } else if ("validValues" in v && "descriptions" in v) {
    return SelectValue(k, v.name, v.validValues, v.descriptions);
  } else if (
    "maxValue" in v &&
    v.maxValue <= 100 &&
    (!v.units || v.units !== "m/s")
  ) {
    const min = v.minValue || 0;
    const max = v.maxValue;
    const numSteps = max - min;
    // Use discrete slider with tick marks for controls with few values (2-10)
    if (numSteps >= 1 && numSteps <= 9) {
      return DiscreteSliderValue(k, v.name, min, max, 0);
    }
    return RangeValue(k, v.name, min, max, 0);
  } else {
    return NumericValue(k, v.name);
  }
}

// ============================================================================
// Control Value Setting (from v1 setControl)
// ============================================================================

function setControlValue(cv) {
  myr_control_values[cv.id] = cv;

  let i = get_element_by_server_id(cv.id);
  let control = getControl(cv.id);
  let units = undefined;
  var value;

  // Update DOM elements if they exist
  if (i && control) {
    if (control.hasAutoAdjustable && cv.auto) {
      value = cv.autoValue;
    } else {
      value = cv.value;
    }

    let html = value;
    if (control.units && cv.id !== "range") {
      [units, value] = toUser(control.units, value);
      if (control.stepValue) {
        value = roundToStep(value, control.stepValue);
      }
      html = value + " " + units;
    }

    // For read-only controls, update the element directly (it's a span with myr_info_value)
    if (control.isReadOnly || control.readOnly) {
      i.innerHTML = html;
    } else if (control && control.dataType === "sector") {
      updateSectorUI(cv.id, control, cv);
    } else {
      // Update numeric display
      let n = document.getElementById(control_prefix + cv.id + "_display");
      if (!n) {
        n = i.parentNode.querySelector(".myr_numeric");
      }
      if (n) {
        n.innerHTML = html;
      }

      // Update description display
      let d = document.getElementById(control_prefix + cv.id + "_desc");
      if (!d) {
        d = i.parentNode.querySelector(".myr_description");
      }
      if (d) {
        let description = control.descriptions
          ? control.descriptions[value]
          : undefined;
        if (!description && control.hasAutoAdjustable) {
          if (cv.auto) {
            description =
              "A" + (value > 0 ? "+" + value : "") + (value < 0 ? value : "");
            i.min = control.autoAdjustMinValue;
            i.max = control.autoAdjustMaxValue;
          } else {
            i.min = control.minValue;
            i.max = control.maxValue;
          }
        }
        if (!description) {
          description = html;
        }
        d.innerHTML = description;
      }

      // Set input value after setting min/max
      i.value = value;

      // Update tick marks for discrete sliders
      if (i.classList.contains("myr_slider_discrete")) {
        updateTickMarks(i);
      }

      // Handle auto toggle button
      if (control.hasAuto && "auto" in cv) {
        let autoBtn = i.parentNode.querySelector(".myr_auto_toggle");
        if (autoBtn) {
          autoBtn.classList.toggle("myr_auto_active", cv.auto);
        }
        let display = cv.auto && !control.hasAutoAdjustable ? "none" : "block";
        if (n) n.style.display = display;
        if (d) d.style.display = display;
        i.style.display = display;
      }

      // Handle enabled checkbox
      if ("enabled" in cv) {
        let checkbox = i.parentNode.querySelector(".myr_enabled");
        if (checkbox) {
          checkbox.checked = cv.enabled;
        }
        let display = cv.enabled ? "block" : "none";
        if (n) n.style.display = display;
        if (d) d.style.display = display;
        i.style.display = display;
      }

      // Special handling for Spoke Processing control - update renderer
      if (cv.id === "spokeProcessing") {
        const rendererModule = window.renderer;
        if (rendererModule && rendererModule.setProcessingMode) {
          // Map server values (0=Clean, 1=Smoothing) to renderer modes
          const mode = cv.value === 0 ? "clean" : "smoothing";
          rendererModule.setProcessingMode(mode);
        }
      }

      // Handle allowed/disallowed state
      if (cv.hasOwnProperty("allowed")) {
        let p = i.parentNode;
        if (!cv.allowed) {
          p.classList.add("myr_readonly");
          i.disabled = true;
        } else {
          p.classList.remove("myr_readonly");
          i.disabled = false;
        }
      }
    }

    // Show error if present
    if (cv.error && myr_error_message) {
      myr_error_message.raise(cv.error);
    }
  }

  // Always notify control callbacks (even if no DOM element exists)
  controlCallbacks.forEach((cb) => {
    cb(cv.id, cv);
  });
}

// ============================================================================
// Event Handlers (from v1)
// ============================================================================

function do_change(v) {
  let id = html_to_server_id(v.id);

  let control = getControl(id);
  let update = myr_control_values[id];
  let message = {};
  let value = v.value;

  if ("user_units" in control && id !== "range") {
    message.units = control.user_units;
    value = Number(value);
  }

  // Check if auto mode is active from current control state
  let auto = update?.auto || false;
  update.auto = auto;
  message.auto = auto;
  if (auto && control.hasAutoAdjustable) {
    update.autoValue = value;
    message.autoValue = value;
  } else {
    update.value = value;
    message.value = value;
  }

  let checkbox = document.getElementById(v.id + enabled_postfix);
  if (checkbox) {
    update.enabled = checkbox.checked;
    message.enabled = checkbox.checked;
  }

  setControlValue(update);
  sendControlToServer(id, message);
}

function do_toggle_auto(btn) {
  let id = html_to_server_id(btn.id);

  let update = myr_control_values[id] || { id: id };
  let newAuto = !update.auto;
  update.auto = newAuto;
  setControlValue(update);

  sendControlToServer(id, { id: id, auto: newAuto });
}

function do_change_enabled(checkbox) {
  let v = document.getElementById(html_to_value_id(checkbox.id));
  do_change(v);
}

function do_button(e) {
  let v = e.target.previousElementSibling;
  let id = html_to_server_id(v.id);
  sendControlToServer(id, { id: id, value: v.value });
}

function do_input() {
  // Real-time feedback while dragging (optional)
}

async function sendControlToServer(controlId, message) {
  if (playbackMode) {
    console.log(`Playback mode: ignoring control ${controlId}`);
    return;
  }

  console.log(`Sending control: ${controlId} = ${JSON.stringify(message)}`);

  const success = await apiSetControl(radarId, controlId, message);
}

// ============================================================================
// ID Conversion Helpers
// ============================================================================

function get_element_by_server_id(id) {
  let did = control_prefix + id;
  return document.getElementById(did);
}

function html_to_server_id(id) {
  let r = id;
  if (r.startsWith(control_prefix)) {
    r = r.substr(control_prefix.length);
  }
  return html_to_value_id(r);
}

function html_to_value_id(id) {
  let r = id;
  if (r.endsWith(auto_postfix)) {
    r = r.substr(0, r.length - auto_postfix.length);
  }
  if (r.endsWith(enabled_postfix)) {
    r = r.substr(0, r.length - enabled_postfix.length);
  }
  return r;
}

// ============================================================================
// WebSocket State Streaming (v2 Signal K protocol)
// ============================================================================

let reconnectAttempts = 0;
const MAX_RECONNECT_DELAY = 30000;
const BASE_RECONNECT_DELAY = 1000;

function connectStateStream(streamUrl, radarIdParam) {
  if (stateWebSocket) {
    stateWebSocket.close();
    stateWebSocket = null;
  }

  const streamUrlWithParams = streamUrl.includes("?")
    ? `${streamUrl}&subscribe=none`
    : `${streamUrl}?subscribe=none`;

  console.log(`Connecting to state stream: ${streamUrlWithParams}`);

  stateWebSocket = new WebSocket(streamUrlWithParams);

  stateWebSocket.onopen = () => {
    console.log("State stream connected");
    reconnectAttempts = 0;
  };

  stateWebSocket.onmessage = (event) => {
    try {
      const message = JSON.parse(event.data);

      if (message.updates) {
        for (const update of message.updates) {
          if (update.meta) {
            for (const item of update.meta) {
              const pathParts = item.path.split(".");
              if (
                pathParts.length === 4 &&
                pathParts[0] === "radars" &&
                pathParts[1] === radarIdParam &&
                pathParts[2] === "controls"
              ) {
                const controlId = pathParts[pathParts.length - 1];

                let control = convertControlToUserUnits(controlId, item.value);
                let newc = JSON.stringify(control);
                let oldc = JSON.stringify(myr_capabilities.controls[controlId]);
                if (oldc != newc) {
                  console.log(
                    `meta data changed: ${controlId} from ${oldc} to ${newc}`
                  );
                  myr_capabilities.controls[controlId] = control;
                } else {
                  console.log(`No change to meta data for ${controlId}`);
                }
              }
            }
          }
          if (update.values) {
            for (const item of update.values) {
              const pathParts = item.path.split(".");
              if (
                pathParts.length === 4 &&
                pathParts[0] === "radars" &&
                pathParts[1] === radarIdParam &&
                pathParts[2] === "controls"
              ) {
                const controlId = pathParts[pathParts.length - 1];

                console.log(
                  `Receiving control value: ${controlId} = ${JSON.stringify(
                    item.value
                  )}`
                );

                const cv = { ...item.value, id: controlId };
                setControlValue(cv);
              }
            }
          }
        }
      } else if (message.name && message.version) {
        console.log("Connected to " + message.name + " v" + message.version);

        const subscription = {
          subscribe: [
            {
              path: `radars.${radarIdParam}.controls.*`,
              policy: "instant",
            },
          ],
        };

        console.log("Subscribing to radar controls:", subscription);
        stateWebSocket.send(JSON.stringify(subscription));
      }
    } catch (err) {
      console.error("Error processing state stream message:", err);
    }
  };

  stateWebSocket.onerror = (error) => {
    console.error("State stream error:", error);
  };

  stateWebSocket.onclose = () => {
    console.log("State stream closed");
    stateWebSocket = null;

    reconnectAttempts++;
    const delay = Math.min(
      BASE_RECONNECT_DELAY * Math.pow(2, reconnectAttempts - 1),
      MAX_RECONNECT_DELAY
    );

    console.log(
      `Reconnecting state stream in ${delay}ms (attempt ${reconnectAttempts})`
    );
    setTimeout(() => {
      if (radarId) {
        connectStateStream(streamUrl, radarIdParam);
      }
    }, delay);
  };
}

function disconnectStateStream() {
  if (stateWebSocket) {
    stateWebSocket.close();
    stateWebSocket = null;
  }
  reconnectAttempts = 0;
}

// ============================================================================
// Initialization
// ============================================================================

setTimeout(() => {
  if (radarCallbacks.length === 0) {
    window.onload = function () {
      const urlParams = new URLSearchParams(window.location.search);
      const id = urlParams.get("id");
      loadRadar(id);
    };
  }
}, 0);

async function loadRadar(id) {
  try {
    await detectMode();

    if (!id) {
      const ids = await fetchRadarIds();
      if (ids.length > 0) {
        id = ids[0];
      }
    }

    if (!id) {
      console.error("No radar found");
      showError("No radar found. Please check connection.");
      setTimeout(() => loadRadar(null), 10000);
      return;
    }

    radarId = id;
    playbackMode = isPlaybackRadar(id);
    console.log(
      `Loading radar: ${radarId}${playbackMode ? " (playback mode)" : ""}`
    );

    const radars = await fetchRadars();
    const radarInfo = radars[radarId];

    myr_capabilities = await fetchCapabilities(radarId);
    console.log("Capabilities:", myr_capabilities);

    // Convert to user units
    myr_capabilities.controls = convertControlsToUserUnits(
      myr_capabilities.controls || {}
    );
    myr_error_message = new TemporaryMessage("myr_error");

    // Build UI
    buildControls();

    // Connect to state stream
    let controlStreamUrl = radarInfo?.streamUrl;
    if (!controlStreamUrl) {
      const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
      if (isStandaloneMode()) {
        controlStreamUrl = `${wsProtocol}//${window.location.host}/v3/api/stream`;
      } else {
        controlStreamUrl = `${wsProtocol}//${window.location.host}/signalk/v2/api/vessels/self/radar/stream`;
      }
    }
    connectStateStream(controlStreamUrl, radarId);

    // Get spokeDataUrl
    let spokeDataUrl = radarInfo?.spokeDataUrl;
    if (!spokeDataUrl) {
      const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
      spokeDataUrl = `${wsProtocol}//${window.location.host}/signalk/v2/api/vessels/self/radars/${radarId}/stream`;
    }

    // Notify callbacks
    radarCallbacks.forEach((cb) =>
      cb({
        id: radarId,
        name: `${myr_capabilities.make} ${myr_capabilities.model}`,
        capabilities: myr_capabilities,
        spokeDataUrl: spokeDataUrl,
      })
    );
  } catch (err) {
    console.error("Failed to load radar:", err);
    showError(`Failed to load radar: ${err.message}`);
    setTimeout(() => loadRadar(id), 10000);
  }
}

function showError(message) {
  const errorEl = document.getElementById("myr_error");
  if (errorEl) {
    errorEl.textContent = message;
    errorEl.style.visibility = "visible";
    setTimeout(() => {
      errorEl.style.visibility = "hidden";
    }, 5000);
  }
}

// ============================================================================
// Exported Helper Functions
// ============================================================================

function getControl(controlId) {
  return myr_capabilities.controls[controlId];
}

function getPowerState() {
  return myr_control_values.power?.value || 0;
}

function convertTimeToSeconds(value, units) {
  switch (units) {
    case "h":
      return value * 3600;
    case "min":
      return value * 60;
    case "s":
    default:
      return value;
  }
}

function getOperatingTime() {
  const onTimeUnits = getControl("operatingTime")?.units || "s";
  const txTimeUnits = getControl("transmitTime")?.units || "s";

  return {
    onTime: convertTimeToSeconds(
      myr_control_values.operatingTime?.value || 0,
      onTimeUnits
    ),
    txTime: convertTimeToSeconds(
      myr_control_values.transmitTime?.value || 0,
      txTimeUnits
    ),
  };
}

function isPlaybackMode() {
  return playbackMode;
}

function getUserName() {
  return myr_control_values.userName?.value || "";
}

function nextValidValue(controlId, currentValue) {
  const control = getControl(controlId);
  if (!control) return currentValue;

  // If control has explicit validValues, cycle through those
  if (control.validValues && control.validValues.length > 0) {
    const validValues = control.validValues;

    // Find the index of current value in validValues (handle type mismatch by comparing as numbers)
    const currentIndex = validValues.findIndex(
      (v) => Number(v) === Number(currentValue)
    );

    // Cycle to next value in validValues
    // If current value is not in validValues, start at first valid value
    const nextIndex =
      currentIndex < 0 ? 0 : (currentIndex + 1) % validValues.length;

    return validValues[nextIndex];
  }

  // Otherwise use minValue/maxValue/stepValue
  const min = control.minValue ?? 0;
  const max = control.maxValue ?? 1;
  const step = control.stepValue ?? 1;

  let nextValue = Number(currentValue) + step;
  if (nextValue > max) {
    nextValue = min;
  }

  return nextValue;
}

function togglePower() {
  const currentValue = myr_control_values.power?.value ?? 0;
  const nextValue = nextValidValue("power", currentValue);

  // Send the control update
  sendControlToServer("power", { value: nextValue });
}

// ============================================================================
// Range Zoom Functions
// ============================================================================

/**
 * Get current range value and valid values
 */
function getRangeInfo() {
  const controlId = "range";

  const control = myr_capabilities.controls[controlId];
  const currentValue = myr_control_values[controlId]?.value;
  const validValues = control?.validValues || [];

  return { controlId, control, currentValue, validValues };
}

/**
 * Zoom in - go to shorter range (previous in validValues, smaller value)
 */
function zoomIn() {
  const info = getRangeInfo();
  if (!info || info.validValues.length === 0) return;

  const { controlId, currentValue, validValues } = info;

  // Find current index
  const currentIndex = validValues.findIndex(
    (v) => Number(v) === Number(currentValue)
  );

  // Go to previous (shorter) range
  if (currentIndex > 0) {
    const newValue = validValues[currentIndex - 1];
    sendControlToServer(controlId, { value: newValue });
  }
}

/**
 * Zoom out - go to longer range (next in validValues, larger value)
 */
function zoomOut() {
  const info = getRangeInfo();
  if (!info || info.validValues.length === 0) return;

  const { controlId, currentValue, validValues } = info;

  // Find current index
  const currentIndex = validValues.findIndex(
    (v) => Number(v) === Number(currentValue)
  );

  // Go to next (longer) range
  if (currentIndex < validValues.length - 1) {
    const newValue = validValues[currentIndex + 1];
    sendControlToServer(controlId, { value: newValue });
  }
}

/**
 * Get current range display text
 */
function getCurrentRangeDisplay() {
  const info = getRangeInfo();
  if (!info) return "";

  const { control, currentValue } = info;
  if (control?.descriptions && control.descriptions[currentValue]) {
    return control.descriptions[currentValue];
  }
  return currentValue ? `${currentValue} m` : "";
}
