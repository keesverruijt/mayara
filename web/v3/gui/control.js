/**
 * Capabilities-Driven Radar Control Panel
 *
 * Dynamically builds the control UI based on radar capabilities from the v5 API.
 * No hardcoded controls - everything is generated from the capability manifest.
 *
 * UI Design: Touch-friendly with sliders and buttons only (no dropdowns).
 */

export {
  loadRadar,
  registerRadarCallback,
  registerControlCallback,
  setCurrentRange,
  getPowerState,
  getOperatingTime,
  hasTimeCapability,
  isPlaybackMode,
};

import van from "./van-1.5.2.js";
import {
  fetchRadars,
  fetchRadarIds,
  fetchCapabilities,
  setControl,
  detectMode,
  isStandaloneMode,
  saveInstallationSetting,
  isPlaybackRadar,
} from "./api.js";

const { div, label, input, button, span } = van.tags;

// State
let radarId = null;
let capabilities = null;
let radarState = null;
let stateWebSocket = null;
let radarCallbacks = [];
let controlCallbacks = [];
let playbackMode = false; // True when viewing a playback radar (controls disabled)

// Current range (for viewer.js integration)
let currentRange = 1852;
let lastRangeUpdateTime = 0;
let rangeUpdateCount = {}; // Track how often each range value is seen
let userRequestedRangeIndex = -1; // Track user's position in range table
let rangeFromSpokeData = false; // True once we've received range from spoke data

// Track pending control changes to prevent polling from overwriting user input
// Maps controlId -> { value, timestamp }
let pendingControls = {};

/**
 * Normalize capabilities.controls to always be a HashMap
 * Converts array format (v2) to object format for consistent access
 */
function normalizeControls(controls) {
  if (!controls) {
    return {};
  }

  // If already an object (v3 format), return as-is
  if (!Array.isArray(controls)) {
    return controls;
  }

  // Convert array (v2 format) to object keyed by controlId
  const controlsMap = {};
  for (const control of controls) {
    const controlId = control.controlId || control.id;
    if (controlId) {
      controlsMap[controlId] = {
        ...control,
        controlId, // Ensure controlId is set
      };
    }
  }
  return controlsMap;
}

function registerRadarCallback(callback) {
  radarCallbacks.push(callback);
}

function registerControlCallback(callback) {
  controlCallbacks.push(callback);
}

// Called from viewer.js when spoke data contains range
// Uses majority voting to prevent flickering from mixed range values during transitions
function setCurrentRange(meters) {
  if (meters <= 0) return;

  const now = Date.now();

  // Reset counts if more than 2 seconds since last update
  if (now - lastRangeUpdateTime > 2000) {
    rangeUpdateCount = {};
  }
  lastRangeUpdateTime = now;

  // Count this range value
  rangeUpdateCount[meters] = (rangeUpdateCount[meters] || 0) + 1;

  // Find the most common range value (need at least 5 samples)
  let maxCount = 0;
  let dominantRange = currentRange;
  for (const [range, count] of Object.entries(rangeUpdateCount)) {
    if (count > maxCount) {
      maxCount = count;
      dominantRange = parseInt(range);
    }
  }

  // Only update if we have a clear majority (5+ samples) and it's different
  if (maxCount >= 5 && dominantRange !== currentRange) {
    currentRange = dominantRange;
    rangeFromSpokeData = true; // Mark that we have real range from radar
    // Also update userRequestedRangeIndex to match spoke data
    const ranges = capabilities?.supportedRanges || [];
    const newIndex = ranges.findIndex((r) => Math.abs(r - dominantRange) < 50);
    if (newIndex >= 0) {
      userRequestedRangeIndex = newIndex;
    }
    rangeUpdateCount = {}; // Reset after accepting new range
    updateRangeDisplay();
  }
}

// ============================================================================
// UI Building from Capabilities
// ============================================================================

/**
 * Build the entire control panel from capabilities
 */
function buildControlsFromCapabilities() {
  const titleEl = document.getElementById("myr_title");
  const controlsEl = document.getElementById("myr_controls");

  if (!capabilities || !controlsEl) return;

  // Set title
  if (titleEl) {
    titleEl.innerHTML = "";
    const titleText = `${capabilities.make || ""} ${
      capabilities.model || ""
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

  // Clear controls
  controlsEl.innerHTML = "";

  // Build radar info header showing model, serial, firmware, etc.
  const infoItems = [];
  if (capabilities.model) {
    infoItems.push({ label: "Model", value: capabilities.model });
  }
  if (capabilities.serialNumber) {
    infoItems.push({ label: "Serial", value: capabilities.serialNumber });
  }
  if (capabilities.firmwareVersion) {
    infoItems.push({ label: "Firmware", value: capabilities.firmwareVersion });
  }
  if (capabilities.maxRange) {
    const maxNm = (capabilities.maxRange / 1852).toFixed(0);
    infoItems.push({ label: "Max Range", value: `${maxNm} nm` });
  }
  if (capabilities.hasDoppler) {
    infoItems.push({ label: "Doppler", value: "Yes" });
  }

  if (infoItems.length > 0) {
    const infoHeader = div(
      { class: "myr_radar_info_header" },
      ...infoItems.map((item) =>
        div(
          { class: "myr_radar_info_item" },
          span({ class: "myr_info_label" }, item.label + ":"),
          span({ class: "myr_info_value" }, item.value)
        )
      )
    );
    van.add(controlsEl, infoHeader);
  }

  // Group controls by category
  // capabilities.controls is now always a HashMap (normalized on load)
  const baseControls = [];
  const extendedControls = [];
  const configControls = [];
  const infoControls = [];

  // Convert HashMap to array for iteration
  for (const [controlId, control] of Object.entries(
    capabilities.controls || {}
  )) {
    // Ensure control has controlId field set
    const fullControl = { ...control, controlId };

    if (fullControl.readOnly) {
      infoControls.push(fullControl);
    } else if (fullControl.category === "installation") {
      configControls.push(fullControl);
    } else if (fullControl.category === "advanced") {
      extendedControls.push(fullControl);
    } else {
      // Default: base category and any unhandled categories
      baseControls.push(fullControl);
    }
  }

  // Sort controls by their numeric id field
  const sortById = (a, b) => (a.id || 0) - (b.id || 0);
  baseControls.sort(sortById);
  extendedControls.sort(sortById);
  configControls.sort(sortById);
  infoControls.sort(sortById);

  // Build base controls (range, gain, sea, rain, etc.)
  if (baseControls.length > 0) {
    const baseSection = div({ class: "myr_control_section" });

    // Range control (special handling with +/- buttons)
    const rangeControl = baseControls.find((c) => c.controlId === "range");
    if (rangeControl) {
      van.add(baseSection, buildRangeControl(rangeControl));
    }

    // Other base controls
    for (const control of baseControls) {
      if (control.controlId !== "range") {
        van.add(baseSection, buildControl(control));
      }
    }

    van.add(controlsEl, baseSection);
  }

  // Build extended controls in a collapsible section
  if (extendedControls.length > 0) {
    const extSection = div(
      { class: "myr_control_section myr_extended_section" },
      div({ class: "myr_section_header" }, "Advanced Controls")
    );

    for (const control of extendedControls) {
      van.add(extSection, buildControl(control));
    }

    van.add(controlsEl, extSection);
  }

  // Build installation controls (config settings - rarely changed)
  if (configControls.length > 0) {
    const configSection = div(
      { class: "myr_control_section myr_installation_section" },
      div({ class: "myr_section_header" }, "Installation")
    );

    for (const control of configControls) {
      van.add(configSection, buildControl(control));
    }

    van.add(controlsEl, configSection);
  }

  // Build info controls (read-only)
  if (infoControls.length > 0) {
    const infoSection = div(
      { class: "myr_control_section myr_info_section" },
      div({ class: "myr_section_header" }, "Radar Information")
    );

    for (const control of infoControls) {
      van.add(infoSection, buildInfoControl(control));
    }

    van.add(controlsEl, infoSection);
  }

  // Apply initial state
  if (radarState) {
    applyStateToUI(radarState);
  }
}

/**
 * Build a control widget based on its type and schema
 */
function buildControl(control) {
  // Special case for dopplerMode - needs custom UI (enabled toggle + mode selector)
  if (control.controlId === "dopplerMode") {
    return buildDopplerModeControl(control);
  }

  // Special case for noTransmitZones - needs custom UI (2 zone editors)
  if (control.controlId === "noTransmitZones") {
    return buildNoTransmitZonesControl(control);
  }

  switch (control.dataType) {
    case "boolean":
      return buildBooleanControl(control);
    case "button":
      return buildButtonControl(control);
    case "string":
      return buildStringControl(control);
    case "number":
      return buildNumberControl(control);
    case "enum":
      return buildEnumControl(control);
    case "compound":
      return buildCompoundControl(control);
    default:
      console.warn(
        `Unknown control dataType: ${control.dataType} for ${control.controlId}`
      );
      return div();
  }
}

/**
 * Range control - +/- buttons with display
 */
function buildRangeControl(control) {
  // Get supported ranges from characteristics
  const ranges = capabilities.supportedRanges || [];

  return div(
    { class: "myr_range_buttons" },
    button(
      {
        type: "button",
        class: "myr_range_button",
        onclick: () => changeRange(-1),
      },
      "Range -"
    ),
    button(
      {
        type: "button",
        class: "myr_range_button",
        onclick: () => changeRange(1),
      },
      "Range +"
    )
  );
}

/**
 * Boolean control - toggle button
 */
function buildBooleanControl(control) {
  const currentValue =
    getControlValue(control.controlId) || control.default || false;

  return div(
    { class: "myr_control myr_boolean_control" },
    span({ class: "myr_control_label" }, control.name),
    button(
      {
        type: "button",
        id: `myr_${control.controlId}`,
        class: `myr_toggle_button ${currentValue ? "myr_toggle_active" : ""}`,
        onclick: (e) => {
          const isActive = e.target.classList.contains("myr_toggle_active");
          sendControlValue(control.controlId, !isActive);
        },
      },
      currentValue ? "On" : "Off"
    )
  );
}

/**
 * Button control - action button
 */
function buildButtonControl(control) {
  return div(
    { class: "myr_control myr_button_control" },
    button(
      {
        type: "button",
        id: `myr_${control.controlId}`,
        class: "myr_action_button",
        onclick: () => {
          sendControlValue(control.controlId, null);
        },
      },
      control.name
    )
  );
}

/**
 * String control - read-only string display
 */
function buildStringControl(control) {
  const value = getControlValue(control.controlId) || "-";

  return div(
    { class: "myr_control myr_string_control myr_readonly" },
    span({ class: "myr_control_label" }, control.name),
    span({ id: `myr_${control.controlId}`, class: "myr_string_value" }, value)
  );
}

/**
 * Number control - slider
 */
function buildNumberControl(control) {
  const range = control.range || { min: 0, max: 100 };
  let currentValue = getControlValue(control.controlId);

  // Handle compound values (objects with mode/value)
  if (typeof currentValue === "object" && currentValue !== null) {
    currentValue = currentValue.value;
  }

  const value =
    currentValue !== undefined ? currentValue : control.default || range.min;

  return div(
    { class: "myr_control myr_number_control" },
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, control.name),
      span(
        { id: `myr_${control.controlId}_value`, class: "myr_control_value" },
        formatNumberValue(value, control)
      )
    ),
    input({
      type: "range",
      id: `myr_${control.controlId}`,
      class: "myr_slider",
      min: range.min,
      max: range.max,
      step: range.step || 1,
      value: value,
      oninput: (e) => {
        // Update display while dragging
        const valEl = document.getElementById(`myr_${control.controlId}_value`);
        if (valEl) {
          valEl.textContent = formatNumberValue(
            parseInt(e.target.value),
            control
          );
        }
      },
      onchange: (e) => {
        sendControlValue(control.controlId, parseInt(e.target.value));
      },
    })
  );
}

/**
 * Enum control - row of buttons (no dropdown per user request)
 */
function buildEnumControl(control) {
  const descriptions = control.descriptions || {};

  // Convert descriptions object {0: "Off", 1: "Normal", 2: "Approaching"} to array of [value, label] pairs
  const valueEntries = Object.entries(descriptions).map(([key, label]) => [
    parseInt(key),
    label,
  ]);

  return div(
    { class: "myr_control myr_enum_control" },
    span({ class: "myr_control_label" }, control.name),
    div(
      { class: "myr_button_group", id: `myr_${control.controlId}_group` },
      ...valueEntries.map(([value, label]) => {
        return button(
          {
            type: "button",
            class: "myr_enum_button",
            "data-value": value,
            onclick: () => sendControlValue(control.controlId, value),
          },
          label
        );
      })
    )
  );
}

/**
 * Compound control - mode selector + value slider (e.g., gain with auto/manual)
 */
function buildCompoundControl(control) {
  const modes = control.modes || ["auto", "manual"];
  const currentState = getControlValue(control.controlId) || {};
  const currentMode = currentState.mode || control.defaultMode || modes[0];
  const currentValue =
    currentState.value !== undefined ? currentState.value : 50;

  // Get value range from properties
  const valueProps = control.properties?.value || {};
  const range = valueProps.range || { min: 0, max: 100 };

  const isAuto = currentMode === "auto";

  return div(
    {
      class: "myr_control myr_compound_control",
      id: `myr_${control.controlId}_compound`,
    },
    div(
      { class: "myr_compound_header" },
      span({ class: "myr_control_label" }, control.name),
      span(
        { id: `myr_${control.controlId}_value`, class: "myr_control_value" },
        isAuto ? "Auto" : currentValue
      )
    ),
    div(
      { class: "myr_compound_body" },
      // Mode buttons
      div(
        { class: "myr_mode_buttons" },
        ...modes.map((mode) =>
          button(
            {
              type: "button",
              class: `myr_mode_button ${
                mode === currentMode ? "myr_mode_active" : ""
              }`,
              "data-mode": mode,
              onclick: () => {
                const slider = document.getElementById(
                  `myr_${control.controlId}_slider`
                );
                const value = slider ? parseInt(slider.value) : currentValue;
                sendControlValue(control.controlId, { mode, value });
              },
            },
            mode.charAt(0).toUpperCase() + mode.slice(1)
          )
        )
      ),
      // Value slider (disabled when auto)
      input({
        type: "range",
        id: `myr_${control.controlId}_slider`,
        class: "myr_slider myr_compound_slider",
        min: range.min,
        max: range.max,
        step: range.step || 1,
        value: currentValue,
        disabled: isAuto,
        oninput: (e) => {
          const valEl = document.getElementById(
            `myr_${control.controlId}_value`
          );
          // Check current mode dynamically, not the captured isAuto
          const modeEl = document.querySelector(
            `#myr_${control.controlId}_compound .myr_mode_active`
          );
          const currentMode = modeEl?.dataset.mode || "auto";
          if (valEl && currentMode !== "auto") {
            valEl.textContent = e.target.value;
          }
        },
        onchange: (e) => {
          // Check current mode dynamically
          const modeEl = document.querySelector(
            `#myr_${control.controlId}_compound .myr_mode_active`
          );
          const mode = modeEl?.dataset.mode || "manual";
          if (mode !== "auto") {
            sendControlValue(control.controlId, {
              mode,
              value: parseInt(e.target.value),
            });
          }
        },
      })
    )
  );
}

/**
 * Doppler Mode control - 3 buttons: Off | Target | Rain
 * Furuno Target Analyzer: { enabled: bool, mode: "target" | "rain" }
 */
function buildDopplerModeControl(control) {
  const currentState = getControlValue(control.controlId) || {
    enabled: false,
    mode: "target",
  };
  const enabled = currentState.enabled || false;
  const mode = currentState.mode || "target";

  // Determine which button is active: off, target, or rain
  const activeBtn = !enabled ? "off" : mode;

  return div(
    { class: "myr_control", id: `myr_${control.controlId}_compound` },
    span({ class: "myr_control_label" }, control.name),
    div(
      { class: "myr_mode_buttons myr_mode_buttons_3" },
      button(
        {
          type: "button",
          class: `myr_mode_button ${
            activeBtn === "off" ? "myr_mode_active" : ""
          }`,
          "data-value": "off",
          onclick: () =>
            sendControlValue(control.controlId, {
              enabled: false,
              mode: "target",
            }),
        },
        "Off"
      ),
      button(
        {
          type: "button",
          class: `myr_mode_button ${
            activeBtn === "target" ? "myr_mode_active" : ""
          }`,
          "data-value": "target",
          onclick: () =>
            sendControlValue(control.controlId, {
              enabled: true,
              mode: "target",
            }),
        },
        "Target"
      ),
      button(
        {
          type: "button",
          class: `myr_mode_button ${
            activeBtn === "rain" ? "myr_mode_active" : ""
          }`,
          "data-value": "rain",
          onclick: () =>
            sendControlValue(control.controlId, {
              enabled: true,
              mode: "rain",
            }),
        },
        "Rain"
      )
    )
  );
}

/**
 * No-Transmit Zones control - 2 zone editors with enabled/start/end
 * Server uses individual controls: noTransmitStart1/End1/Start2/End2
 * Value of -1 means zone is disabled
 */
function buildNoTransmitZonesControl(control) {
  // Read from individual controls (server uses flat model)
  // -1 means zone is disabled
  const z1Start = getControlValue("noTransmitStart1") ?? -1;
  const z1End = getControlValue("noTransmitEnd1") ?? -1;
  const z2Start = getControlValue("noTransmitStart2") ?? -1;
  const z2End = getControlValue("noTransmitEnd2") ?? -1;

  // -1 means disabled (value < 0)
  const zone1 = {
    enabled: z1Start >= 0 && z1End >= 0,
    start: z1Start < 0 ? 0 : z1Start,
    end: z1End < 0 ? 0 : z1End,
  };
  const zone2 = {
    enabled: z2Start >= 0 && z2End >= 0,
    start: z2Start < 0 ? 0 : z2Start,
    end: z2End < 0 ? 0 : z2End,
  };

  // Read current zone values from DOM (to avoid stale closure values)
  function getZoneFromDOM(zoneNum) {
    const prefix = `myr_ntz_zone${zoneNum}`;
    const enabledEl = document.getElementById(`${prefix}_enabled`);
    const startEl = document.getElementById(`${prefix}_start`);
    const endEl = document.getElementById(`${prefix}_end`);
    return {
      enabled: enabledEl?.checked || false,
      start: parseInt(startEl?.value) || 0,
      end: parseInt(endEl?.value) || 0,
    };
  }

  function sendCurrentZones() {
    const z1 = getZoneFromDOM(1);
    const z2 = getZoneFromDOM(2);
    console.log("NTZ: Sending zones:", { z1, z2 });

    // Send individual control values (server has noTransmitStart1/End1/Start2/End2)
    // When zone is disabled, send -1 for both angles (server convention for disabled)
    const z1Start = z1.enabled ? z1.start : -1;
    const z1End = z1.enabled ? z1.end : -1;
    const z2Start = z2.enabled ? z2.start : -1;
    const z2End = z2.enabled ? z2.end : -1;

    // Send all four controls using sendControlValue to get pending tracking
    sendControlValue("noTransmitStart1", z1Start);
    sendControlValue("noTransmitEnd1", z1End);
    sendControlValue("noTransmitStart2", z2Start);
    sendControlValue("noTransmitEnd2", z2End);
  }

  function buildZoneEditor(zoneNum, zone) {
    const prefix = `myr_ntz_zone${zoneNum}`;

    // Handler for checkbox change - enable/disable inputs and send
    function onEnabledChange(e) {
      const enabled = e.target.checked;
      const startEl = document.getElementById(`${prefix}_start`);
      const endEl = document.getElementById(`${prefix}_end`);
      if (startEl) startEl.disabled = !enabled;
      if (endEl) endEl.disabled = !enabled;
      sendCurrentZones();
    }

    return div(
      { class: "myr_ntz_zone" },
      div(
        { class: "myr_ntz_zone_header" },
        label(
          { class: "myr_checkbox_label" },
          input({
            type: "checkbox",
            id: `${prefix}_enabled`,
            checked: zone.enabled,
            onchange: onEnabledChange,
          }),
          ` Zone ${zoneNum}`
        )
      ),
      div(
        { class: "myr_ntz_angles" },
        div(
          { class: "myr_ntz_angle" },
          label({ for: `${prefix}_start` }, "Start°"),
          input({
            type: "number",
            id: `${prefix}_start`,
            min: 0,
            max: 359,
            value: zone.start,
            disabled: !zone.enabled,
            onchange: () => sendCurrentZones(),
          })
        ),
        div(
          { class: "myr_ntz_angle" },
          label({ for: `${prefix}_end` }, "End°"),
          input({
            type: "number",
            id: `${prefix}_end`,
            min: 0,
            max: 359,
            value: zone.end,
            disabled: !zone.enabled,
            onchange: () => sendCurrentZones(),
          })
        )
      )
    );
  }

  return div(
    {
      class: "myr_control myr_ntz_control",
      id: `myr_${control.controlId}_compound`,
    },
    span({ class: "myr_control_label" }, control.name),
    div(
      { class: "myr_ntz_zones" },
      buildZoneEditor(1, zone1),
      buildZoneEditor(2, zone2)
    )
  );
}

/**
 * Read-only info control
 */
function buildInfoControl(control) {
  const value = getControlValue(control.controlId) || "-";

  return div(
    { class: "myr_control myr_info_control" },
    span({ class: "myr_control_label" }, control.name),
    span(
      { id: `myr_${control.controlId}`, class: "myr_info_value" },
      formatInfoValue(value, control)
    )
  );
}

// ============================================================================
// Control Value Helpers
// ============================================================================

function getControlValue(controlId) {
  return radarState[controlId];
}

function formatNumberValue(value, control) {
  // Handle compound values (objects with mode/value)
  let numValue = value;
  if (typeof value === "object" && value !== null) {
    if (value.mode === "auto") {
      return "Auto";
    }
    numValue = value.value !== undefined ? value.value : 0;
  }

  const unit = control?.range?.unit || "";
  if (unit === "percent") {
    return `${numValue}%`;
  }
  return unit ? `${numValue} ${unit}` : String(numValue);
}

function formatInfoValue(value, control) {
  if (control.units && control.units === "s") {
    if (value % 3600 == 0) {
      return String(value / 3600) + " h";
    }
  }
  return String(value);
}

function formatRange(meters) {
  const nm = meters / 1852;
  if (nm >= 1) {
    if (nm === 1.5) return "1.5 nm";
    return Math.round(nm) + " nm";
  } else if (nm >= 0.7) {
    return "3/4 nm";
  } else if (nm >= 0.4) {
    return "1/2 nm";
  } else if (nm >= 0.2) {
    return "1/4 nm";
  } else if (nm >= 0.1) {
    return "1/8 nm";
  } else {
    return "1/16 nm";
  }
}

function updateRangeDisplay() {
  const display = document.getElementById("myr_range_display");
  if (display) {
    display.textContent = formatRange(currentRange);
  }
}

// ============================================================================
// Control Commands
// ============================================================================

async function sendControlValue(controlId, value) {
  if (!radarId) return;

  // Don't send control commands to playback radars
  if (playbackMode) {
    console.log(`Playback mode: ignoring control ${controlId}`);
    return;
  }

  console.log(`Sending control: ${controlId} = ${JSON.stringify(value)}`);

  // Mark as pending to prevent polling from overwriting
  pendingControls[controlId] = { value, timestamp: Date.now() };

  // Optimistic UI update immediately
  updateControlUI(controlId, value);

  const success = await setControl(radarId, controlId, value);

  if (success) {
    // Notify callbacks (capabilities.controls is a HashMap)
    const control = capabilities?.controls?.[controlId];
    controlCallbacks.forEach((cb) => cb(control, { id: controlId, value }));

    // Persist Installation category controls (write-only settings like bearingAlignment)
    // Use capabilities.key (e.g., "Furuno-RD003212") for storage - compatible with WASM SignalK plugin
    if (control?.category === "installation") {
      const storageKey = capabilities?.key || radarId;
      saveInstallationSetting(storageKey, controlId, value);
    }
  }
}

function changeRange(direction) {
  const ranges = capabilities?.supportedRanges || [];
  if (ranges.length === 0) return;

  // Use tracked index if valid, otherwise find from current range
  if (userRequestedRangeIndex < 0 || userRequestedRangeIndex >= ranges.length) {
    userRequestedRangeIndex = ranges.findIndex(
      (r) => Math.abs(r - currentRange) < 50
    );
    if (userRequestedRangeIndex < 0) userRequestedRangeIndex = 0;
  }

  const newIndex = Math.max(
    0,
    Math.min(ranges.length - 1, userRequestedRangeIndex + direction)
  );
  const newRange = ranges[newIndex];

  // Always update index to track user's position
  userRequestedRangeIndex = newIndex;

  sendControlValue("range", newRange);
}

// ============================================================================
// UI Updates from State
// ============================================================================

function updateControlUI(controlId, value) {
  // Update local state
  radarState[controlId] = value;

  // Update UI based on control type (capabilities.controls is a HashMap)
  const control = capabilities?.controls?.[controlId];
  if (!control) return;

  if (controlId === "power") {
    console.log("*** updateControlUI POWER");
  }
  // Special case for dopplerMode
  if (controlId === "dopplerMode") {
    updateDopplerModeUI(controlId, value);
    return;
  }

  // Special case for noTransmitZones (compound) or individual NTZ controls
  if (controlId === "noTransmitZones") {
    updateNoTransmitZonesUI(value);
    return;
  }
  // Handle individual NTZ controls - update the compound UI
  if (controlId.startsWith("noTransmit")) {
    updateNoTransmitZoneFromIndividual(controlId, value);
    return;
  }

  switch (control.dataType) {
    case "boolean":
      updateBooleanUI(controlId, value);
      break;
    case "string":
      updateStringUI(controlId, value);
      break;
    case "number":
      updateNumberUI(controlId, value, control);
      break;
    case "enum":
      updateEnumUI(controlId, value);
      break;
    case "compound":
      updateCompoundUI(controlId, value, control);
      break;
  }
}

function updateBooleanUI(controlId, value) {
  const btn = document.getElementById(`myr_${controlId}`);
  const boolValue = value.value;
  if (btn) {
    btn.classList.toggle("myr_toggle_active", boolValue);
    btn.textContent = value ? "On" : "Off";
  }
}

function updateStringUI(controlId, value, control) {
  const valueEl = document.getElementById(`myr_${controlId}_value`);

  if (valueEl) {
    valueEl.textContent = value.value;
  }
}

function updateNumberUI(controlId, value, control) {
  const slider = document.getElementById(`myr_${controlId}`);
  const valueEl = document.getElementById(`myr_${controlId}_value`);

  if (slider) {
    slider.value = value.value;
  }
  if (valueEl) {
    valueEl.textContent = formatNumberValue(value.value, control);
  }
}

function updateEnumUI(controlId, value) {
  const group = document.getElementById(`myr_${controlId}_group`);
  if (group) {
    // Convert value to string for comparison (dataset values are always strings)
    const valueStr = String(value.value);
    group.querySelectorAll(".myr_enum_button").forEach((btn) => {
      btn.classList.toggle("myr_enum_active", btn.dataset.value == valueStr);
    });
  }
}

function updateCompoundUI(controlId, value, control) {
  const compound = document.getElementById(`myr_${controlId}_compound`);
  if (!compound) return;

  const mode = value?.mode || "auto";
  const val = value?.value;

  // Update mode buttons
  compound.querySelectorAll(".myr_mode_button").forEach((btn) => {
    btn.classList.toggle("myr_mode_active", btn.dataset.mode === mode);
  });

  // Update slider
  const slider = compound.querySelector(".myr_compound_slider");
  const valueEl = document.getElementById(`myr_${controlId}_value`);

  const isAuto = mode === "auto";
  if (slider) {
    slider.disabled = isAuto;
    if (val !== undefined) {
      slider.value = val;
    }
  }
  if (valueEl) {
    valueEl.textContent = isAuto ? "Auto" : val !== undefined ? val : "-";
  }
}

function updateDopplerModeUI(controlId, value) {
  const compound = document.getElementById(`myr_${controlId}_compound`);
  if (!compound) return;

  const enabled = value?.enabled || false;
  const mode = value?.mode || "target";
  const activeBtn = !enabled ? "off" : mode;

  // Update buttons (Off / Target / Rain)
  compound.querySelectorAll(".myr_mode_button").forEach((btn) => {
    btn.classList.toggle("myr_mode_active", btn.dataset.value === activeBtn);
  });
}

function updateNoTransmitZonesUI(value) {
  const zones = value?.zones || [];
  const zone1 = zones[0] || { enabled: false, start: 0, end: 0 };
  const zone2 = zones[1] || { enabled: false, start: 0, end: 0 };

  // Update zone 1
  const z1Enabled = document.getElementById("myr_ntz_zone1_enabled");
  const z1Start = document.getElementById("myr_ntz_zone1_start");
  const z1End = document.getElementById("myr_ntz_zone1_end");
  if (z1Enabled) z1Enabled.checked = zone1.enabled;
  if (z1Start) {
    z1Start.value = zone1.start;
    z1Start.disabled = !zone1.enabled;
  }
  if (z1End) {
    z1End.value = zone1.end;
    z1End.disabled = !zone1.enabled;
  }

  // Update zone 2
  const z2Enabled = document.getElementById("myr_ntz_zone2_enabled");
  const z2Start = document.getElementById("myr_ntz_zone2_start");
  const z2End = document.getElementById("myr_ntz_zone2_end");
  if (z2Enabled) z2Enabled.checked = zone2.enabled;
  if (z2Start) {
    z2Start.value = zone2.start;
    z2Start.disabled = !zone2.enabled;
  }
  if (z2End) {
    z2End.value = zone2.end;
    z2End.disabled = !zone2.enabled;
  }
}

/**
 * Update NTZ UI from individual control updates (noTransmitStart1, etc.)
 * Server uses flat model with -1 meaning disabled
 */
function updateNoTransmitZoneFromIndividual(controlId, value) {
  // Parse control ID: noTransmitStart1, noTransmitEnd1, noTransmitStart2, noTransmitEnd2
  const match = controlId.match(/noTransmit(Start|End)(\d)/);
  if (!match) return;

  const [, type, zoneNum] = match;
  const prefix = `myr_ntz_zone${zoneNum}`;
  const isStart = type === "Start";

  // -1 means zone is disabled (value < 0)
  const isDisabled = value < 0;
  const displayValue = isDisabled ? 0 : value;

  // Update the angle input
  const inputEl = document.getElementById(
    `${prefix}_${isStart ? "start" : "end"}`
  );
  if (inputEl) {
    inputEl.value = displayValue;
  }

  // Check if both start and end are >= 0 to determine enabled state
  // Use pending values if available, otherwise fall back to state
  const startId = `noTransmitStart${zoneNum}`;
  const endId = `noTransmitEnd${zoneNum}`;
  const startVal =
    pendingControls[startId]?.value ?? getControlValue(startId) ?? -1;
  const endVal = pendingControls[endId]?.value ?? getControlValue(endId) ?? -1;
  const zoneEnabled = startVal >= 0 && endVal >= 0;

  // Update enabled checkbox and input disabled states
  const enabledEl = document.getElementById(`${prefix}_enabled`);
  const startEl = document.getElementById(`${prefix}_start`);
  const endEl = document.getElementById(`${prefix}_end`);

  if (enabledEl) enabledEl.checked = zoneEnabled;
  if (startEl) startEl.disabled = !zoneEnabled;
  if (endEl) endEl.disabled = !zoneEnabled;
}

function applyStateToUI(state) {
  if (!state) return;

  for (const [controlId, value] of Object.entries(state)) {
    // Skip controls with pending changes until server confirms the same value
    const pending = pendingControls[controlId];
    if (pending) {
      // Check if server has confirmed our pending value
      const serverValue = JSON.stringify(value);
      const pendingValue = JSON.stringify(pending.value);
      if (serverValue === pendingValue) {
        // Server confirmed, clear pending
        delete pendingControls[controlId];
      } else {
        // Server hasn't confirmed yet, keep user's value
        continue;
      }
    }
    updateControlUI(controlId, value);
  }

  // Update range display and initialize range index
  // Skip if we already have range from spoke data (more accurate than state API)
  if (state.range && !rangeFromSpokeData) {
    currentRange = state.range;
    // Initialize userRequestedRangeIndex from actual radar range
    const ranges = capabilities?.supportedRanges || [];
    userRequestedRangeIndex = ranges.findIndex(
      (r) => Math.abs(r - currentRange) < 50
    );
    if (userRequestedRangeIndex < 0) userRequestedRangeIndex = 0;
    updateRangeDisplay();
  }
}

// ============================================================================
// State Streaming (WebSocket)
// ============================================================================

let reconnectAttempts = 0;
const MAX_RECONNECT_DELAY = 30000; // Max 30s between reconnects
const BASE_RECONNECT_DELAY = 1000; // Start with 1s

function connectStateStream(streamUrl, radarId) {
  if (stateWebSocket) {
    stateWebSocket.close();
    stateWebSocket = null;
  }

  // Add query parameter for no initial subscription
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

      // Signal K header:
      // { name: "Marine Yacht Radar",
      //   version: "3.0.0",
      //   timestamp: "2026-02-15T17:27:15.750914737+00:00",
      //   roles: ["master"] }

      // Signal K delta format:
      // {
      //   "context": "self",
      //   "updates": [{
      //     "source": { "label": "radar-id", ... },
      //     "values": [{ "path": "radars.<id>.<control>", "value": ... }]
      //   }]
      // }

      if (message.updates) {
        for (const update of message.updates) {
          if (update.values) {
            for (const item of update.values) {
              // Extract control ID from path: radars.<id>.<controlId>
              const pathParts = item.path.split(".");
              if (
                pathParts.length == 4 &&
                pathParts[0] === "radars" &&
                pathParts[1] === radarId &&
                pathParts[2] === "controls"
              ) {
                const controlId = pathParts[pathParts.length - 1];

                // Update radarState
                if (!radarState) radarState = {};
                radarState[controlId] = item.value;

                console.log(
                  `Receiving control: ${controlId} = ${JSON.stringify(
                    item.value
                  )}`
                );

                // Update UI for this control
                applyControlValueToUI(controlId, item.value);
              } else {
                console.log("Dropping unknown SK path " + item.path);
              }
            }
          }
        }
      } else if (message.name && message.version) {
        console.log("Connected to " + message.name + " v" + message.version);

        // Subscribe to this radar's control values
        const subscription = {
          subscribe: [
            {
              path: `radars.${radarId}.controls.*`,
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

    // Attempt to reconnect with exponential backoff
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
        connectStateStream(streamUrl, radarId);
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

// Apply a single control value to the UI
function applyControlValueToUI(controlId, value) {
  // Check if this is a pending control change from the user

  const pending = pendingControls[controlId];
  if (pending) {
    // No no no, controls, come back from other devices as well!
    //   const age = Date.now() - pending.timestamp;
    //   if (age < 2000) {
    //     // Ignore server update if we recently sent a change
    //     return;
    //   }
    delete pendingControls[controlId];
  }

  updateControlUI(controlId, value);

  // Notify control callbacks
  controlCallbacks.forEach((cb) => cb(controlId, value));
}

// ============================================================================
// Initialization (for standalone control.html only)
// ============================================================================

// For control.html: auto-initialize on load
// For viewer.html: viewer.js imports this module and calls loadRadar() itself
// We detect standalone mode by checking if viewer.js has NOT registered a callback
// (viewer.js calls registerRadarCallback before window.onload)
setTimeout(() => {
  // If no callbacks registered after module evaluation, we're in standalone mode
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

    // If no ID provided, get first radar
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

    // Fetch radar info to get spokeDataUrl and streamUrl
    const radars = await fetchRadars();
    const radarInfo = radars[radarId];

    // Fetch capabilities
    capabilities = await fetchCapabilities(radarId);
    console.log("Capabilities:", capabilities);

    // Normalize controls to always be a HashMap for consistent access
    if (capabilities.controls) {
      capabilities.controls = normalizeControls(capabilities.controls);
    }

    // Initialize empty state (will be populated by stream)
    radarState = {};

    // Build UI
    buildControlsFromCapabilities();

    // Connect to state stream for real-time control value updates
    let controlStreamUrl = radarInfo?.streamUrl;
    if (!controlStreamUrl) {
      const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
      if (isStandaloneMode()) {
        // Standalone mode: use v3 stream endpoint
        controlStreamUrl = `${wsProtocol}//${window.location.host}/v3/api/stream`;
      } else {
        // SignalK mode: use default SignalK stream
        controlStreamUrl = `${wsProtocol}//${window.location.host}/signalk/v1/stream`;
      }
    }
    connectStateStream(controlStreamUrl, radarId);

    // Notify callbacks (viewer.js expects these properties)
    const chars = capabilities || {};

    // Get spokeDataUrl from radar info (v3 API provides spokeDataUrl)
    // Fall back to constructing URL for SignalK mode if not available
    let spokeDataUrl = radarInfo?.spokeDataUrl;
    if (!spokeDataUrl) {
      const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
      if (isStandaloneMode()) {
        // Standalone mode fallback (shouldn't happen with v3 API)
        spokeDataUrl = `${wsProtocol}//${window.location.host}/v1/api/spokes/${radarId}`;
      } else {
        // SignalK mode: use /signalk/v2/api/vessels/self/radars/{id}/stream
        spokeDataUrl = `${wsProtocol}//${window.location.host}/signalk/v2/api/vessels/self/radars/${radarId}/stream`;
      }
    }

    radarCallbacks.forEach((cb) =>
      cb({
        id: radarId,
        name: `${capabilities.make} ${capabilities.model}`,
        capabilities,
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

/**
 * Get current power state
 * @returns {number}
 */
function getPowerState() {
  return radarState?.power?.value || 1;
}

/**
 * Get operating time from radar state
 * @returns {{ onTime: number, txTime: number }} time in seconds
 */
function getOperatingTime() {
  return {
    onTime: radarState?.operatingTime?.value || 0,
    txTime: radarState?.transmitTime?.value || 0,
  };
}

/**
 * Check if radar has capability (operatingTime or transmitTime)
 * @returns {{ hasOnTime: boolean, hasTxTime: boolean }}
 */
function hasTimeCapability() {
  // capabilities.controls is a HashMap
  const controls = capabilities?.controls || {};
  return {
    hasOnTime: "operatingTime" in controls,
    hasTxTime: "transmitTime" in controls,
  };
}

/**
 * Check if currently viewing a playback radar (controls are disabled)
 * @returns {boolean} True if in playback mode
 */
function isPlaybackMode() {
  return playbackMode;
}
