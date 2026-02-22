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
import { PPI } from "./ppi.js";

var webSocket;
var headingSocket;
var RadarMessage;
var ppi;  // The PPI display instance
var webgpuRenderer;  // The WebGPU backend renderer
var capabilities;

// Heading mode: "headingUp" or "northUp"
var headingMode = "headingUp";
var trueHeading = 0; // in radians

registerRadarCallback(radarLoaded);
registerControlCallback(controlUpdate);

window.onload = async function () {
  const urlParams = new URLSearchParams(window.location.search);
  const id = urlParams.get("id");

  // Check WebGPU availability
  const webgpuAvailable = await checkWebGPU();
  if (!webgpuAvailable) {
    return; // Error message already shown
  }

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

  // Create WebGPU renderer (backend for spoke rendering)
  webgpuRenderer = new WebGPURenderer(
    document.getElementById("myr_canvas_webgl")
  );

  // Create PPI display (handles overlay, zones, spoke processing)
  ppi = new PPI(
    webgpuRenderer,
    document.getElementById("myr_canvas_overlay"),
    document.getElementById("myr_canvas_background")
  );

  // Wait for WebGPU initialization AND protobuf loading before proceeding
  await Promise.all([webgpuRenderer.initPromise, protobufPromise]);
  console.log("Both WebGPU and protobuf ready");

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

// Check WebGPU and show error if not available
async function checkWebGPU() {
  const hasWebGPUApi = !!navigator.gpu;
  const isSecure = window.isSecureContext;

  if (!hasWebGPUApi) {
    showWebGPUError("no-api", hasWebGPUApi, isSecure);
    return false;
  }

  try {
    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) {
      showWebGPUError("no-adapter", hasWebGPUApi, isSecure);
      return false;
    }
    return true;
  } catch (e) {
    showWebGPUError("adapter-error", hasWebGPUApi, isSecure);
    return false;
  }
}

function showWebGPUError(failureReason, hasWebGPUApi, isSecure) {
  const container = document.querySelector(".myr_container");
  if (!container) return;

  const os = detectOS();
  const browser = detectBrowser();
  const hostname = window.location.hostname;
  const port = window.location.port || "80";

  let errorMessage = "";
  if (failureReason === "no-api" && !isSecure) {
    errorMessage = "WebGPU API not available - likely due to insecure context.";
  } else if (failureReason === "no-api") {
    errorMessage = "WebGPU API not available in this browser.";
  } else if (failureReason === "no-adapter") {
    errorMessage = "No WebGPU adapter found. Your GPU may not support WebGPU.";
  } else {
    errorMessage = "WebGPU initialization failed.";
  }

  container.innerHTML = `
    <div class="myr_webgpu_error">
      <h2>WebGPU Required</h2>
      <p class="myr_error_message">${errorMessage}</p>

      ${
        !isSecure
          ? `
        <div class="myr_error_section">
          <h3>Secure Context Required</h3>
          <p>WebGPU requires a secure context. You are accessing via HTTP on "${hostname}".</p>
          ${getSecureContextOptionsHTML(browser, os, port)}
        </div>
      `
          : ""
      }

      <div class="myr_error_section">
        <h3>Enable WebGPU / Hardware Acceleration</h3>
        ${getBrowserInstructionsHTML(browser, os)}
      </div>

      <div class="myr_error_actions">
        <a href="index.html" class="myr_back_link">Back to Radar List</a>
        <button onclick="location.reload()" class="myr_retry_button">Retry</button>
      </div>
    </div>
  `;
}

function detectOS() {
  const ua = navigator.userAgent.toLowerCase();
  const platform = navigator.platform?.toLowerCase() || "";

  if (ua.includes("iphone") || ua.includes("ipad")) return "ios";
  if (
    navigator.maxTouchPoints > 1 &&
    (ua.includes("mac") || platform.includes("mac"))
  )
    return "ios";
  if (ua.includes("android")) return "android";

  if (ua.includes("win") || platform.includes("win")) return "windows";
  if (ua.includes("mac") || platform.includes("mac")) return "macos";
  if (ua.includes("linux") || platform.includes("linux")) return "linux";
  return "unknown";
}

function detectBrowser() {
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("edg/")) return "edge";
  if (ua.includes("chrome")) return "chrome";
  if (ua.includes("firefox")) return "firefox";
  if (ua.includes("safari") && !ua.includes("chrome")) return "safari";
  return "unknown";
}

function getSecureContextOptionsHTML(browser, os, port) {
  const origin = window.location.origin;
  const isMobile = os === "ios" || os === "android";

  let options = "";

  if (!isMobile) {
    options += `
      <p><strong>Option 1 (easiest):</strong> Access via localhost instead:</p>
      <div class="myr_code_instructions">
        <p><code>http://localhost:${port}</code> or <code>http://127.0.0.1:${port}</code></p>
        <p class="myr_note">Browsers treat localhost as a secure context</p>
      </div>
    `;
  }

  const optNum = isMobile ? 1 : 2;
  options += `
    <p><strong>Option ${optNum}:</strong> Add this site to browser exceptions:</p>
    ${getInsecureOriginHTML(browser, os)}
    <p><strong>Option ${
      optNum + 1
    }:</strong> Use HTTPS (requires server configuration)</p>
  `;

  return options;
}

function getInsecureOriginHTML(browser, os) {
  const origin = window.location.origin;
  const hostname = window.location.hostname;

  if (os === "ios") {
    return `
      <div class="myr_code_instructions">
        <p>Safari on iOS/iPadOS does not support insecure origin exceptions.</p>
        <p>Alternatives:</p>
        <p>• Configure HTTPS on your SignalK server</p>
        <p>• Use a tunneling service (e.g., ngrok) to get an HTTPS URL</p>
        <p>• Access from a desktop browser where you can set the flag</p>
      </div>
    `;
  }

  if (os === "android" && browser === "chrome") {
    return `
      <div class="myr_code_instructions">
        <p>1. Open Chrome on your Android device</p>
        <p>2. Go to: <code>chrome://flags/#unsafely-treat-insecure-origin-as-secure</code></p>
        <p>3. Add: <code>${origin}</code></p>
        <p>4. Set to "Enabled"</p>
        <p>5. Tap "Relaunch"</p>
      </div>
    `;
  }

  if (browser === "chrome" || browser === "edge") {
    const flagPrefix = browser === "edge" ? "edge" : "chrome";
    const flagUrl = `${flagPrefix}://flags/#unsafely-treat-insecure-origin-as-secure`;
    return `
      <div class="myr_code_instructions">
        <p>1. Copy and paste this into your address bar:</p>
        <p><a href="${flagUrl}" class="myr_flag_link"><code>${flagUrl}</code></a></p>
        <p>2. In the text field, add: <code>${origin}</code></p>
        <p>3. Set dropdown to "Enabled"</p>
        <p>4. Click "Relaunch" at the bottom</p>
      </div>
    `;
  }
  if (browser === "firefox") {
    return `
      <div class="myr_code_instructions">
        <p>1. Open: <a href="about:config" class="myr_flag_link"><code>about:config</code></a></p>
        <p>2. Click "Accept the Risk and Continue"</p>
        <p>3. Search for: <code>dom.securecontext.allowlist</code></p>
        <p>4. Click the + button to add: <code>${hostname}</code></p>
        <p>5. Restart Firefox</p>
      </div>
    `;
  }
  return `<p>Check your browser settings for allowing insecure origins.</p>`;
}

function getBrowserInstructionsHTML(browser, os) {
  if (browser === "safari" && os === "ios") {
    return `
      <div class="myr_code_instructions">
        <p>Safari on iOS/iPadOS 17+:</p>
        <p>1. Open the <strong>Settings</strong> app</p>
        <p>2. Scroll down and tap <strong>Safari</strong></p>
        <p>3. Scroll down and tap <strong>Advanced</strong></p>
        <p>4. Tap <strong>Feature Flags</strong></p>
        <p>5. Enable <strong>WebGPU</strong></p>
        <p>6. Return to Safari and reload this page</p>
        <p class="myr_note">Note: Requires iOS/iPadOS 17 or later.</p>
      </div>
    `;
  }

  switch (browser) {
    case "chrome":
      return `
        <div class="myr_code_instructions">
          <p>Chrome should have WebGPU enabled by default (v113+).</p>
          <p>If not working:</p>
          <p>1. Open: <code>chrome://flags/#enable-unsafe-webgpu</code></p>
          <p>2. Set to "Enabled"</p>
          <p>3. Relaunch Chrome</p>
          ${
            os === "linux"
              ? '<p class="myr_note">Linux: Vulkan drivers required.</p>'
              : ""
          }
        </div>
      `;
    case "edge":
      return `
        <div class="myr_code_instructions">
          <p>Edge should have WebGPU enabled by default.</p>
          <p>If not working:</p>
          <p>1. Open: <code>edge://flags/#enable-unsafe-webgpu</code></p>
          <p>2. Set to "Enabled"</p>
          <p>3. Relaunch Edge</p>
        </div>
      `;
    case "firefox":
      return `
        <div class="myr_code_instructions">
          <p>Firefox WebGPU (experimental):</p>
          <p>1. Open: <code>about:config</code></p>
          <p>2. Search: <code>dom.webgpu.enabled</code></p>
          <p>3. Set to: <code>true</code></p>
          <p>4. Restart Firefox</p>
        </div>
      `;
    case "safari":
      return `
        <div class="myr_code_instructions">
          <p>Safari WebGPU (macOS 14+):</p>
          <p>1. Open Safari menu > Settings</p>
          <p>2. Go to Advanced tab</p>
          <p>3. Check "Show features for web developers"</p>
          <p>4. Go to Feature Flags tab</p>
          <p>5. Enable "WebGPU"</p>
          <p>6. Restart Safari</p>
        </div>
      `;
    default:
      return `
        <div class="myr_code_instructions">
          <p>WebGPU requires:</p>
          <p>- Chrome 113+ (recommended)</p>
          <p>- Edge 113+</p>
          <p>- Safari 17+</p>
          <p>- Firefox (experimental)</p>
        </div>
      `;
  }
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
  if (!ppi || !webgpuRenderer || !webgpuRenderer.ready) {
    pendingRadarData = r;
    return;
  }

  // Initialize PPI with radar capabilities
  ppi.setLegend(capabilities.legend);
  ppi.setSpokes(spokesPerRevolution, maxSpokeLength);

  // Also initialize renderer with spokes (for texture sizing)
  webgpuRenderer.setSpokes(spokesPerRevolution, maxSpokeLength);

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
    const powerState =
      getControl(controlId).descriptions[value.value].toLowerCase();
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
