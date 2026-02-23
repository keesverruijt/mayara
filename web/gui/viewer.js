"use strict";

export {
  setZoneEditMode,
  setSectorEditMode,
};

import {
  loadRadar,
  getControl,
  registerRadarCallback,
  registerControlCallback,
  getOperatingTime,
  getUserName,
  togglePower,
  zoomIn,
  zoomOut,
  getCurrentRangeDisplay,
} from "./control.js";
import { isStandaloneMode, detectMode } from "./api.js";
import "./protobuf/protobuf.min.js";

import { WebGPURenderer } from "./render_webgpu.js";
import { WebGLRenderer } from "./render_webgl.js";
import { PPI } from "./ppi.js";

var webSocket;
var headingSocket;
var RadarMessage;
var ppi;  // The PPI display instance
var renderer;  // The backend renderer (WebGPU or WebGL)
var capabilities;
var renderMethod = "webgpu";  // "webgpu" or "webgl"

// Heading mode: "headingUp" or "northUp"
var headingMode = "headingUp";
var trueHeading = 0; // in radians

registerRadarCallback(radarLoaded);
registerControlCallback(controlUpdate);

window.onload = async function () {
  const urlParams = new URLSearchParams(window.location.search);
  const id = urlParams.get("id");
  const requestedRenderer = urlParams.get("renderer");

  // Determine which renderer to use
  renderMethod = await selectRenderer(requestedRenderer);
  if (!renderMethod) {
    return; // Error message already shown
  }

  console.log(`Using ${renderMethod} renderer`);

  // Load protobuf definition - must complete before websocket can process messages
  const protobufPromise = new Promise((resolve, reject) => {
    protobuf.load("./proto/RadarMessage.proto", function (err, root) {
      if (err) {
        reject(err);
        return;
      }
      RadarMessage = root.lookupType(".RadarMessage");
      console.log("RadarMessage protobuf loaded successfully");
      resolve();
    });
  });

  // Create renderer based on selected method
  const canvas = document.getElementById("myr_canvas_webgl");
  if (renderMethod === "webgpu") {
    renderer = new WebGPURenderer(canvas);
  } else {
    renderer = new WebGLRenderer(canvas);
  }

  // Create PPI display (handles overlay, zones, spoke processing)
  ppi = new PPI(
    renderer,
    document.getElementById("myr_canvas_overlay"),
    document.getElementById("myr_canvas_background")
  );

  // Wait for renderer initialization AND protobuf loading before proceeding
  await Promise.all([renderer.initPromise, protobufPromise]);
  console.log(`Both ${renderMethod} and protobuf ready`);

  // Debug: expose ppi globally for console debugging
  window.ppi = ppi;
  window.renderer = ppi; // Backwards compatibility

  // Process any pending radar data that arrived before renderer was ready
  if (pendingRadarData) {
    console.log("Processing deferred radar data");
    radarLoaded(pendingRadarData);
    pendingRadarData = null;
  } else {
    // No pending data - load radar now
    loadRadar(id);
  }

  // Subscribe to SignalK heading delta (only in SignalK mode)
  subscribeToHeading();

  // Create hamburger menu button and setup controls toggle
  createHamburgerMenu();

  // Create heading mode toggle button
  createHeadingModeToggle();

  // Create power lozenge
  createPowerLozenge();

  // Create range lozenge
  createRangeLozenge();

  window.onresize = function () {
    ppi.redrawCanvas();
  };
};

// Subscribe to navigation.headingTrue via SignalK WebSocket
function subscribeToHeading() {
  if (isStandaloneMode()) {
    console.log("Standalone mode: heading subscription disabled (no SignalK)");
    return;
  }

  const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const streamUrl = `${wsProtocol}//${window.location.host}/signalk/v1/stream?subscribe=none`;

  headingSocket = new WebSocket(streamUrl);

  headingSocket.onopen = () => {
    console.log("Heading WebSocket connected");
    const subscription = {
      context: "vessels.self",
      subscribe: [
        {
          path: "navigation.headingTrue",
          period: 200,
        },
      ],
    };
    headingSocket.send(JSON.stringify(subscription));
  };

  headingSocket.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data);
      if (data.updates) {
        for (const update of data.updates) {
          if (update.values) {
            for (const value of update.values) {
              if (value.path === "navigation.headingTrue") {
                trueHeading = value.value; // Already in radians
                updateHeadingDisplay();
              }
            }
          }
        }
      }
    } catch (e) {
      // Ignore parse errors (e.g., hello message)
    }
  };

  headingSocket.onerror = (e) => {
    console.log("Heading WebSocket error:", e);
  };

  headingSocket.onclose = () => {
    console.log("Heading WebSocket closed, reconnecting in 5s...");
    setTimeout(subscribeToHeading, 5000);
  };
}

// Update PPI with current heading
function updateHeadingDisplay(mode) {
  if (ppi) {
    ppi.setTrueHeading(trueHeading);
    if (mode) {
      return ppi.setHeadingMode(mode);
    }
  }
  return mode || headingMode;
}

// Create the heading mode toggle button
function createHeadingModeToggle() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  const toggleBtn = document.createElement("div");
  toggleBtn.id = "myr_heading_toggle";
  toggleBtn.className = "myr_heading_toggle";
  toggleBtn.innerHTML = "H Up";
  toggleBtn.title = "Click to toggle: Heading Up / North Up";

  toggleBtn.addEventListener("click", () => {
    if (headingMode === "headingUp") {
      headingMode = "northUp";
      toggleBtn.innerHTML = "N Up";
    } else {
      headingMode = "headingUp";
      toggleBtn.innerHTML = "H Up";
    }
    headingMode = updateHeadingDisplay(headingMode);
    if (headingMode === "headingUp") {
      toggleBtn.innerHTML = "H Up";
    } else {
      toggleBtn.innerHTML = "N Up";
    }
    ppi.redrawCanvas();
  });

  container.appendChild(toggleBtn);
}

// Create the power lozenge on the viewer
function createPowerLozenge() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  const lozenge = document.createElement("div");
  lozenge.id = "myr_power_lozenge";
  lozenge.className = "myr_power_lozenge myr_power_off";
  lozenge.title = "Click power icon to toggle radar power";

  const powerBtn = document.createElement("button");
  powerBtn.className = "myr_power_lozenge_button";
  powerBtn.innerHTML = `<svg class="myr_power_icon" viewBox="0 0 24 24">
    <path d="M12 3v9"/>
    <path d="M18.4 6.6a9 9 0 1 1-12.8 0"/>
  </svg>`;
  powerBtn.addEventListener("click", () => {
    togglePower();
  });

  const nameDisplay = document.createElement("div");
  nameDisplay.id = "myr_power_lozenge_name";
  nameDisplay.className = "myr_power_lozenge_name";
  nameDisplay.textContent = getUserName() || "Radar";

  lozenge.appendChild(powerBtn);
  lozenge.appendChild(nameDisplay);
  container.appendChild(lozenge);
}

// Update the power lozenge state
function updatePowerLozenge(powerState, userName) {
  const lozenge = document.getElementById("myr_power_lozenge");
  if (!lozenge) return;

  if (powerState !== undefined) {
    lozenge.classList.remove(
      "myr_power_transmit",
      "myr_power_standby",
      "myr_power_off"
    );
    if (powerState === "transmit") {
      lozenge.classList.add("myr_power_transmit");
    } else if (powerState === "standby") {
      lozenge.classList.add("myr_power_standby");
    } else {
      lozenge.classList.add("myr_power_off");
    }
  }

  if (userName !== undefined) {
    const nameDisplay = document.getElementById("myr_power_lozenge_name");
    if (nameDisplay) {
      nameDisplay.textContent = userName || "Radar";
    }
  }
}

// Create the range lozenge on the viewer
function createRangeLozenge() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  const lozenge = document.createElement("div");
  lozenge.id = "myr_range_lozenge";
  lozenge.className = "myr_range_lozenge";
  lozenge.title = "Click + to zoom in, - to zoom out";

  const zoomInBtn = document.createElement("div");
  zoomInBtn.className = "myr_range_zoom";
  zoomInBtn.innerHTML = "+";
  zoomInBtn.addEventListener("click", () => {
    zoomIn();
  });

  const rangeDisplay = document.createElement("div");
  rangeDisplay.id = "myr_range_display";
  rangeDisplay.className = "myr_range_display";
  rangeDisplay.textContent = "";

  const zoomOutBtn = document.createElement("div");
  zoomOutBtn.className = "myr_range_zoom";
  zoomOutBtn.innerHTML = "−";
  zoomOutBtn.addEventListener("click", () => {
    zoomOut();
  });

  lozenge.appendChild(zoomInBtn);
  lozenge.appendChild(rangeDisplay);
  lozenge.appendChild(zoomOutBtn);
  container.appendChild(lozenge);
}

// Update the range display
function updateRangeDisplay() {
  const rangeDisplay = document.getElementById("myr_range_display");
  if (rangeDisplay) {
    rangeDisplay.textContent = getCurrentRangeDisplay();
  }
}

// Create the hamburger menu button and setup controls toggle
function createHamburgerMenu() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  // Create hamburger button
  const hamburgerBtn = document.createElement("button");
  hamburgerBtn.type = "button";
  hamburgerBtn.id = "myr_hamburger_button";
  hamburgerBtn.className = "myr_hamburger_button";
  hamburgerBtn.title = "Open radar controls";

  // Three lines for hamburger icon
  for (let i = 0; i < 3; i++) {
    const line = document.createElement("span");
    line.className = "myr_hamburger_line";
    hamburgerBtn.appendChild(line);
  }

  // Get references to controls panel and close button
  const controller = document.getElementById("myr_controller");
  const closeBtn = document.getElementById("myr_close_controls");

  // Toggle controls panel open
  hamburgerBtn.addEventListener("click", () => {
    if (controller) {
      controller.classList.add("myr_controller_open");
    }
  });

  // Close controls panel
  if (closeBtn) {
    closeBtn.addEventListener("click", () => {
      if (controller) {
        controller.classList.remove("myr_controller_open");
      }
    });
  }

  container.appendChild(hamburgerBtn);
}

// Check WebGPU availability
async function checkWebGPU() {
  if (!navigator.gpu) return false;
  try {
    const adapter = await navigator.gpu.requestAdapter();
    return !!adapter;
  } catch (e) {
    return false;
  }
}

// Check WebGL2 availability
function checkWebGL() {
  const canvas = document.createElement("canvas");
  const gl = canvas.getContext("webgl2");
  return !!gl;
}

// Select renderer based on query parameter and availability
// Returns "webgpu", "webgl", or null if neither available
async function selectRenderer(requested) {
  const webgpuAvailable = await checkWebGPU();
  const webglAvailable = checkWebGL();

  // If specific renderer requested, try to use it
  if (requested === "webgl") {
    if (webglAvailable) return "webgl";
    showRendererError("WebGL2");
    return null;
  }
  if (requested === "webgpu") {
    if (webgpuAvailable) return "webgpu";
    showRendererError("WebGPU");
    return null;
  }

  // Auto-select: prefer WebGPU, fallback to WebGL
  if (webgpuAvailable) return "webgpu";
  if (webglAvailable) return "webgl";

  // Neither available
  showRendererError("WebGPU or WebGL2");
  return null;
}

function showRendererError(rendererName) {
  const container = document.querySelector(".myr_container");
  if (!container) return;

  container.innerHTML = `
    <div class="myr_webgpu_error">
      <h2>${rendererName} Not Available</h2>
      <p class="myr_error_message">This display requires ${rendererName} which is not available in your browser.</p>

      <div class="myr_error_section">
        <h3>Possible Solutions</h3>
        <div class="myr_code_instructions">
          <p>Try one of the following:</p>
          <p>- Use a modern browser (Chrome, Firefox, Edge, Safari)</p>
          <p>- Enable hardware acceleration in browser settings</p>
          <p>- Update your graphics drivers</p>
          <p>- See the <a href="index.html" class="myr_flag_link">radar list page</a> for detailed setup instructions</p>
        </div>
      </div>

      <div class="myr_error_actions">
        <a href="index.html" class="myr_back_link">Back to Radar List</a>
        <button onclick="location.reload()" class="myr_retry_button">Retry</button>
      </div>
    </div>
  `;
}

function restart(id) {
  setTimeout(loadRadar, 15000, id);
}

// Pending radar data if callback arrives before PPI is ready
var pendingRadarData = null;

// r contains id, name, capabilities and spokeDataUrl
function radarLoaded(r) {
  capabilities = r.capabilities;
  let maxSpokeLength = capabilities.maxSpokeLength;
  let spokesPerRevolution = capabilities.spokesPerRevolution;
  let prev_angle = -1;

  // If PPI isn't ready yet, store data and return
  if (!ppi || !renderer || !renderer.ready) {
    pendingRadarData = r;
    return;
  }

  // Initialize PPI with radar capabilities
  ppi.setLegend(capabilities.legend);
  ppi.setSpokes(spokesPerRevolution, maxSpokeLength);

  // Also initialize renderer with spokes (for texture sizing)
  renderer.setSpokes(spokesPerRevolution, maxSpokeLength);

  // Use provided spokeDataUrl or construct SignalK stream URL
  let spokeDataUrl = r.spokeDataUrl;
  if (
    !spokeDataUrl ||
    spokeDataUrl === "undefined" ||
    spokeDataUrl === "null"
  ) {
    const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    spokeDataUrl = `${wsProtocol}//${window.location.host}/signalk/v2/api/vessels/self/radars/${r.id}/stream`;
  } else {
    spokeDataUrl = spokeDataUrl.replace("{id}", r.id);
  }
  console.log("Connecting to radar stream:", spokeDataUrl);
  webSocket = new WebSocket(spokeDataUrl);
  webSocket.binaryType = "arraybuffer";

  webSocket.onopen = (e) => {
    console.log("websocket open: " + JSON.stringify(e));
  };
  webSocket.onclose = (e) => {
    console.log(
      "websocket close: code=" +
        e.code +
        ", reason=" +
        e.reason +
        ", wasClean=" +
        e.wasClean
    );
    restart(r.id);
  };
  webSocket.onerror = (e) => {
    console.log("websocket error:", e);
  };
  webSocket.onmessage = (e) => {
    try {
      const dataSize = e.data?.byteLength || e.data?.length || 0;
      if (dataSize === 0) {
        console.warn("WS message received with 0 bytes");
        return;
      }
      if (!RadarMessage) {
        console.warn("RadarMessage not loaded yet, dropping message");
        return;
      }
      let buf = e.data;
      let bytes = new Uint8Array(buf);
      var message = RadarMessage.decode(bytes);
      if (message.spokes && message.spokes.length > 0) {
        for (let i = 0; i < message.spokes.length; i++) {
          let spoke = message.spokes[i];
          ppi.drawSpoke(spoke);
          prev_angle = spoke.angle;
        }
        ppi.render();
      }
    } catch (err) {
      console.error("Error processing WebSocket message:", err);
    }
  };
}

function controlUpdate(controlId, value) {
  if (controlId === "power") {
    const control = getControl(controlId);
    // Default to "off" if control not loaded yet or description not found
    let powerState = "off";
    if (control?.descriptions && value.value in control.descriptions) {
      powerState = control.descriptions[value.value].toLowerCase();
    }
    if (ppi) {
      const time = getOperatingTime();
      ppi.setPowerMode(powerState, time.onTime, time.txTime);
    }
    updatePowerLozenge(powerState);
  } else if (controlId === "userName") {
    updatePowerLozenge(undefined, value.value);
  } else if (controlId === "guardZone1") {
    if (ppi) {
      ppi.setGuardZone(0, parseGuardZone(value));
    }
  } else if (controlId === "guardZone2") {
    if (ppi) {
      ppi.setGuardZone(1, parseGuardZone(value));
    }
  } else if (controlId.startsWith("noTransmitSector")) {
    const index = parseInt(controlId.slice(-1)) - 1;
    if (index >= 0 && index < 4 && ppi) {
      ppi.setNoTransmitSector(index, parseNoTransmitSector(value));
    }
  } else {
    const control = getControl(controlId);
    if (control?.name === "Range") {
      const range = typeof value === "object" ? value.value : value;
      ppi.setRange(range);
      updateRangeDisplay();
    }
  }
}

// Parse guard zone control value into drawing parameters
function parseGuardZone(cv) {
  if (!cv || !cv.enabled) return null;
  return {
    startAngle: cv.value ?? 0,
    endAngle: cv.endValue ?? 0,
    startDistance: cv.startDistance ?? 0,
    endDistance: cv.endDistance ?? 0,
  };
}

// Parse no-transmit sector control value into drawing parameters
function parseNoTransmitSector(cv) {
  if (!cv || !cv.enabled) return null;
  return {
    startAngle: cv.value ?? 0,
    endAngle: cv.endValue ?? 0,
  };
}

/**
 * Enable/disable zone edit mode with drag handles on the viewer
 */
function setZoneEditMode(controlId, editing, onDragEnd = null) {
  if (!ppi) return;

  if (!editing) {
    ppi.setEditingZone(null, null);
    return;
  }

  let zoneIndex = null;
  if (controlId === "guardZone1") {
    zoneIndex = 0;
  } else if (controlId === "guardZone2") {
    zoneIndex = 1;
  }

  if (zoneIndex === null) return;

  const wrappedCallback = onDragEnd
    ? (index, zone) => onDragEnd(zone)
    : null;

  ppi.setEditingZone(zoneIndex, wrappedCallback);
}

/**
 * Enable/disable sector edit mode with drag handles on the viewer
 */
function setSectorEditMode(controlId, editing, onDragEnd = null) {
  if (!ppi) return;

  if (!editing) {
    ppi.setEditingSector(null, null);
    return;
  }

  let sectorIndex = null;
  const match = controlId.match(/noTransmitSector(\d)/);
  if (match) {
    sectorIndex = parseInt(match[1]) - 1;
  }

  if (sectorIndex === null || sectorIndex < 0 || sectorIndex > 3) return;

  const wrappedCallback = onDragEnd
    ? (index, sector) => onDragEnd(sector)
    : null;

  ppi.setEditingSector(sectorIndex, wrappedCallback);
}
