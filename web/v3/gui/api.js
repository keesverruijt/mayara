/**
 * API adapter for Mayara Radar v5
 *
 * Automatically detects whether running in SignalK or standalone mode
 * and provides a unified API interface for the capabilities-driven v5 API.
 */

// API endpoints for different modes
const SIGNALK_RADARS_API = "/signalk/v2/api/vessels/self/radars";
const STANDALONE_RADARS_API = "/v2/api/radars";
const STANDALONE_INTERFACES_API = "/v2/api/interfaces";

// Application Data API path - aligned with WASM SignalK plugin
// Uses same path so settings are shared between standalone and SignalK modes
const APPDATA_PATH = "/signalk/v1/applicationData/global/@mayara/signalk-radar";

// Detected mode (null = not detected yet)
let detectedMode = null;

// Cache for capabilities (fetched once per radar)
const capabilitiesCache = new Map();

/**
 * Detect which API mode we're running in
 * @returns {Promise<string>} 'signalk' or 'standalone'
 */
export async function detectMode() {
  if (detectedMode) {
    return detectedMode;
  }

  // Try standalone first - check if /v2/api/radars returns 200
  try {
    const response = await fetch(STANDALONE_RADARS_API, { method: 'HEAD' });
    if (response.ok) {
      detectedMode = 'standalone';
      console.log("Detected standalone mode");
      return detectedMode;
    }
  } catch (e) {
    // Standalone not available
  }

  // Try SignalK - check if endpoint returns 200
  try {
    const response = await fetch(SIGNALK_RADARS_API, { method: 'HEAD' });
    if (response.ok) {
      detectedMode = 'signalk';
      console.log("Detected SignalK mode");
      return detectedMode;
    }
  } catch (e) {
    // SignalK not available
  }

  // Default to standalone
  detectedMode = 'standalone';
  console.log("Defaulting to standalone mode");
  return detectedMode;
}

/**
 * Get the radars API URL for current mode
 * @returns {string} API URL
 */
export function getRadarsUrl() {
  return detectedMode === 'signalk' ? SIGNALK_RADARS_API : STANDALONE_RADARS_API;
}

/**
 * Get the interfaces API URL (standalone only)
 * @returns {string|null} API URL or null if not available
 */
export function getInterfacesUrl() {
  return detectedMode === 'standalone' ? STANDALONE_INTERFACES_API : null;
}

/**
 * Fetch list of radar IDs
 * @returns {Promise<string[]>} Array of radar IDs
 */
export async function fetchRadarIds() {
  await detectMode();

  const response = await fetch(getRadarsUrl());
  const data = await response.json();

  // SignalK v5 returns array of IDs: ["Furuno-RD003212", "Navico-HALO"]
  if (Array.isArray(data)) {
    // Could be array of IDs (strings) or array of radar objects
    if (data.length > 0 && typeof data[0] === 'string') {
      return data;
    }
    // Legacy: array of radar objects
    return data.map(r => r.id);
  }

  // Standalone returns object keyed by ID
  return Object.keys(data);
}

/**
 * Fetch list of radars (legacy compatibility)
 * @returns {Promise<Object>} Radars object keyed by ID
 */
export async function fetchRadars() {
  await detectMode();

  const response = await fetch(getRadarsUrl());
  const data = await response.json();

  // SignalK returns an array, standalone returns an object
  if (detectedMode === 'signalk' && Array.isArray(data)) {
    // Convert array to object keyed by id
    const radars = {};
    for (const radar of data) {
      radars[radar.id] = radar;
    }
    return radars;
  }

  return data;
}

/**
 * Fetch radar capabilities (v5 API)
 * Returns the capability manifest with controls schema, characteristics, etc.
 * @param {string} radarId - The radar ID
 * @returns {Promise<Object>} Capability manifest
 */
export async function fetchCapabilities(radarId) {
  await detectMode();

  // Don't cache capabilities - model info may be updated after TCP connects
  // The radar model is identified via TCP $N96 response, which happens after
  // initial discovery. Caching would return stale "Unknown" model.

  const url = `${getRadarsUrl()}/${radarId}/capabilities`;
  console.log(`Fetching capabilities: GET ${url}`);

  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Failed to fetch capabilities: ${response.status}`);
  }

  return response.json();
}

/**
 * Fetch radar state (v5 API)
 * Returns current values of all controls
 * @param {string} radarId - The radar ID
 * @returns {Promise<Object>} Radar state
 */
export async function fetchState(radarId) {
  await detectMode();

  const url = `${getRadarsUrl()}/${radarId}/state`;

  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Failed to fetch state: ${response.status}`);
  }

  return response.json();
}

/**
 * Clear cached capabilities (e.g., when radar disconnects)
 * @param {string} radarId - The radar ID, or omit to clear all
 */
export function clearCapabilitiesCache(radarId) {
  if (radarId) {
    capabilitiesCache.delete(radarId);
  } else {
    capabilitiesCache.clear();
  }
}

/**
 * Fetch list of interfaces (standalone mode only)
 * @returns {Promise<Object|null>} Interfaces object or null
 */
export async function fetchInterfaces() {
  await detectMode();

  const url = getInterfacesUrl();
  if (!url) {
    return null;
  }

  const response = await fetch(url);
  return response.json();
}

/**
 * Check if we're in SignalK mode
 * @returns {boolean}
 */
export function isSignalKMode() {
  return detectedMode === 'signalk';
}

/**
 * Check if we're in standalone mode
 * @returns {boolean}
 */
export function isStandaloneMode() {
  return detectedMode === 'standalone';
}

/**
 * Map power control values to SignalK RadarStatus
 * SignalK expects: 'off' | 'standby' | 'transmit' | 'warming'
 */
function mapPowerValue(value) {
  // Handle numeric or string values
  const v = String(value);
  if (v === '0' || v === 'off' || v === 'Off') return 'standby';
  if (v === '1' || v === 'on' || v === 'On') return 'transmit';
  // Pass through if already a valid RadarStatus
  if (['off', 'standby', 'transmit', 'warming'].includes(v)) return v;
  return v;
}

/**
 * Send a control command to a radar via REST API (v5 format)
 *
 * SignalK Radar API v5 format:
 *   PUT /signalk/v2/api/vessels/self/radars/{radarId}/controls/{controlId}
 *   Body: { value: ... }
 *
 * @param {string} radarId - The radar ID
 * @param {string} controlId - The control ID (e.g., "power", "gain", "range")
 * @param {any} value - The value to set (type depends on control)
 * @returns {Promise<boolean>} True if successful
 */
export async function setControl(radarId, controlId, value) {
  await detectMode();

  const url = `${getRadarsUrl()}/${radarId}/controls/${controlId}`;
  const body = { value };

  console.log(`Setting control: PUT ${url}`, body);

  try {
    const response = await fetch(url, {
      method: 'PUT',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(body),
    });

    if (response.ok) {
      console.log(`Control ${controlId} set successfully`);
      return true;
    } else {
      const errorText = await response.text();
      console.error(`Control command failed: ${response.status} ${response.statusText} for ${url}`, errorText);
      return false;
    }
  } catch (e) {
    console.error(`Control command error: ${e}`);
    return false;
  }
}

/**
 * Get installation settings for a radar from Application Data API
 * Uses same path as WASM SignalK plugin: @mayara/signalk-radar/1.0.0
 * Structure: { "radars": { "radar-id": { "bearingAlignment": ..., ... } } }
 * @param {string} radarId - The radar ID
 * @returns {Promise<Object>} Installation settings object for this radar
 */
export async function getInstallationSettings(radarId) {
  const url = `${APPDATA_PATH}/1.0.0`;
  try {
    const response = await fetch(url);
    if (!response.ok) return {};
    const data = await response.json();
    return data?.radars?.[radarId] || {};
  } catch (e) {
    console.warn('Failed to load installation settings:', e.message);
    return {};
  }
}

/**
 * Save an installation setting to Application Data API
 * Preserves the nested structure used by WASM SignalK plugin
 * @param {string} radarId - The radar ID
 * @param {string} key - The setting key (e.g., "bearingAlignment")
 * @param {any} value - The value to save
 * @returns {Promise<boolean>} True if successful
 */
export async function saveInstallationSetting(radarId, key, value) {
  const url = `${APPDATA_PATH}/1.0.0`;
  try {
    // Load full structure (preserve other radars' settings)
    const getResponse = await fetch(url);
    const data = getResponse.ok ? await getResponse.json() : { radars: {} };

    // Update nested value
    if (!data.radars) data.radars = {};
    if (!data.radars[radarId]) data.radars[radarId] = {};
    data.radars[radarId][key] = value;

    // Save back
    const putResponse = await fetch(url, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(data)
    });
    if (putResponse.ok) {
      console.log(`Installation setting '${key}' saved for ${radarId}`);
      return true;
    } else {
      console.error(`Failed to save installation setting: ${putResponse.status}`);
      return false;
    }
  } catch (e) {
    console.error('Failed to save installation setting:', e);
    return false;
  }
}

// ============================================================================
// Playback Detection
// ============================================================================

/**
 * Check if a radar is a playback radar (virtual radar from recording playback)
 * @param {string} radarId - The radar ID
 * @returns {boolean} True if this is a playback radar
 */
export function isPlaybackRadar(radarId) {
  return radarId && radarId.startsWith('playback-');
}

// ============================================================================
// Recordings API
// ============================================================================

const RECORDINGS_API = "/v2/api/recordings";

/**
 * List available recordings
 * @param {string} [subdirectory] - Optional subdirectory to list
 * @returns {Promise<Object[]>} Array of recording info objects
 */
export async function listRecordings(subdirectory) {
  const url = subdirectory
    ? `${RECORDINGS_API}/files?dir=${encodeURIComponent(subdirectory)}`
    : `${RECORDINGS_API}/files`;
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Failed to list recordings: ${response.status}`);
  }
  const data = await response.json();
  // Server returns { recordings: [...], totalCount, totalSize }
  return data.recordings || [];
}

/**
 * Get recording file info
 * @param {string} filename - The recording filename
 * @param {string} [subdirectory] - Optional subdirectory
 * @returns {Promise<Object>} Recording info object
 */
export async function getRecordingInfo(filename, subdirectory) {
  const params = subdirectory ? `?dir=${encodeURIComponent(subdirectory)}` : '';
  const response = await fetch(`${RECORDINGS_API}/files/${encodeURIComponent(filename)}${params}`);
  if (!response.ok) {
    throw new Error(`Failed to get recording info: ${response.status}`);
  }
  return response.json();
}

/**
 * Delete a recording
 * @param {string} filename - The recording filename
 * @param {string} [subdirectory] - Optional subdirectory
 * @returns {Promise<boolean>} True if successful
 */
export async function deleteRecording(filename, subdirectory) {
  const params = subdirectory ? `?dir=${encodeURIComponent(subdirectory)}` : '';
  const response = await fetch(`${RECORDINGS_API}/files/${encodeURIComponent(filename)}${params}`, {
    method: 'DELETE'
  });
  return response.ok;
}

/**
 * Rename a recording file
 * @param {string} oldFilename - Current filename
 * @param {string} newFilename - New filename
 * @returns {Promise<Object>} Result with new filename
 */
export async function renameRecording(oldFilename, newFilename) {
  const response = await fetch(`${RECORDINGS_API}/files/${encodeURIComponent(oldFilename)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ newName: newFilename })
  });

  if (!response.ok) {
    const error = await response.json().catch(() => ({ error: 'Rename failed' }));
    throw new Error(error.error || 'Rename failed');
  }

  // Server returns empty body on success
  return { success: true, newFilename };
}

/**
 * Upload a recording file (.mrr or .mrr.gz)
 * @param {File} file - The file to upload
 * @returns {Promise<Object>} Upload result with filename and size
 */
export async function uploadRecording(file) {
  const response = await fetch(`${RECORDINGS_API}/files/upload`, {
    method: 'POST',
    headers: {
      'Content-Disposition': `attachment; filename="${file.name}"`
    },
    body: file
  });

  if (!response.ok) {
    const error = await response.json().catch(() => ({ error: 'Upload failed' }));
    throw new Error(error.error || 'Upload failed');
  }

  return await response.json();
}

/**
 * Get the download URL for a recording (returns compressed .mrr.gz file)
 * @param {string} filename - Recording filename
 * @param {string} [subdirectory] - Optional subdirectory
 * @returns {string} Download URL
 */
export function getRecordingDownloadUrl(filename, subdirectory) {
  const params = subdirectory ? `?dir=${encodeURIComponent(subdirectory)}` : '';
  return `${RECORDINGS_API}/files/${encodeURIComponent(filename)}/download${params}`;
}

/**
 * Get list of radars available for recording
 * @returns {Promise<Object[]>} Array of radar info objects
 */
export async function getRecordableRadars() {
  const response = await fetch(`${RECORDINGS_API}/radars`);
  if (!response.ok) {
    throw new Error(`Failed to get recordable radars: ${response.status}`);
  }
  return response.json();
}

/**
 * Start recording from a radar
 * @param {string} radarId - The radar ID to record
 * @param {string} [filename] - Optional filename (auto-generated if not provided)
 * @param {string} [subdirectory] - Optional subdirectory
 * @returns {Promise<Object>} Recording status
 */
export async function startRecording(radarId, filename, subdirectory) {
  const body = { radarId };
  if (filename) body.filename = filename;
  if (subdirectory) body.subdirectory = subdirectory;

  const response = await fetch(`${RECORDINGS_API}/record/start`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body)
  });
  if (!response.ok) {
    const error = await response.text();
    throw new Error(`Failed to start recording: ${error}`);
  }
  return response.json();
}

/**
 * Stop the current recording
 * @returns {Promise<Object>} Final recording status
 */
export async function stopRecording() {
  const response = await fetch(`${RECORDINGS_API}/record/stop`, {
    method: 'POST'
  });
  if (!response.ok) {
    throw new Error(`Failed to stop recording: ${response.status}`);
  }
  return response.json();
}

/**
 * Get current recording status
 * @returns {Promise<Object>} Recording status
 */
export async function getRecordingStatus() {
  const response = await fetch(`${RECORDINGS_API}/record/status`);
  if (!response.ok) {
    throw new Error(`Failed to get recording status: ${response.status}`);
  }
  return response.json();
}

/**
 * Load a recording for playback
 * @param {string} filename - The recording filename
 * @param {string} [subdirectory] - Optional subdirectory
 * @returns {Promise<Object>} Playback status with radarId
 */
export async function loadPlayback(filename, subdirectory) {
  const body = { filename };
  if (subdirectory) body.subdirectory = subdirectory;

  const response = await fetch(`${RECORDINGS_API}/playback/load`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body)
  });
  if (!response.ok) {
    const error = await response.text();
    throw new Error(`Failed to load recording: ${error}`);
  }
  return response.json();
}

/**
 * Start/resume playback
 * @returns {Promise<Object>} Playback status
 */
export async function playPlayback() {
  const response = await fetch(`${RECORDINGS_API}/playback/play`, {
    method: 'POST'
  });
  if (!response.ok) {
    throw new Error(`Failed to start playback: ${response.status}`);
  }
  return response.json();
}

/**
 * Pause playback
 * @returns {Promise<Object>} Playback status
 */
export async function pausePlayback() {
  const response = await fetch(`${RECORDINGS_API}/playback/pause`, {
    method: 'POST'
  });
  if (!response.ok) {
    throw new Error(`Failed to pause playback: ${response.status}`);
  }
  return response.json();
}

/**
 * Stop playback and unload
 * @returns {Promise<Object>} Playback status
 */
export async function stopPlayback() {
  const response = await fetch(`${RECORDINGS_API}/playback/stop`, {
    method: 'POST'
  });
  if (!response.ok) {
    throw new Error(`Failed to stop playback: ${response.status}`);
  }
  return response.json();
}

/**
 * Seek to position in playback
 * @param {number} positionMs - Position in milliseconds
 * @returns {Promise<Object>} Playback status
 */
export async function seekPlayback(positionMs) {
  const response = await fetch(`${RECORDINGS_API}/playback/seek`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ positionMs })
  });
  if (!response.ok) {
    throw new Error(`Failed to seek: ${response.status}`);
  }
  return response.json();
}

/**
 * Update playback settings
 * @param {Object} settings - Settings object { speed?, loopPlayback? }
 * @returns {Promise<Object>} Playback status
 */
export async function setPlaybackSettings(settings) {
  const response = await fetch(`${RECORDINGS_API}/playback/settings`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(settings)
  });
  if (!response.ok) {
    throw new Error(`Failed to update playback settings: ${response.status}`);
  }
  return response.json();
}

/**
 * Get current playback status
 * @returns {Promise<Object>} Playback status
 */
export async function getPlaybackStatus() {
  const response = await fetch(`${RECORDINGS_API}/playback/status`);
  if (!response.ok) {
    throw new Error(`Failed to get playback status: ${response.status}`);
  }
  return response.json();
}

// ============================================================================
// Legacy Control API
// ============================================================================

export async function sendControlCommand(radarId, controlData, controls) {
  await detectMode();

  // Map control id to control name for the endpoint
  // controlData.id is the control key (e.g., "1" for Power)
  const controlDef = controls ? controls[controlData.id] : null;
  const controlName = controlDef ? controlDef.name.toLowerCase() : `control-${controlData.id}`;

  const url = `${getRadarsUrl()}/${radarId}/${controlName}`;

  // Build the request body based on controlData and control type
  let body;
  if (controlName === 'power') {
    // Power expects RadarStatus string
    body = { value: mapPowerValue(controlData.value) };
  } else if (controlName === 'range') {
    // Range expects number in meters
    body = { value: parseFloat(controlData.value) };
  } else if (controlName === 'gain' || controlName === 'sea' || controlName === 'rain') {
    // Gain/sea/rain expect { auto: boolean, value?: number }
    body = {};
    if ('auto' in controlData) {
      body.auto = controlData.auto;
    }
    if (controlData.value !== undefined) {
      body.value = parseFloat(controlData.value);
    }
  } else {
    // Generic control
    body = { value: controlData.value };
    if ('auto' in controlData) {
      body.auto = controlData.auto;
    }
  }

  console.log(`Sending control: PUT ${url}`, body);

  try {
    const response = await fetch(url, {
      method: 'PUT',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(body),
    });

    if (response.ok) {
      console.log(`Control command sent successfully: PUT ${url}`);
      return true;
    } else {
      const errorText = await response.text();
      console.error(`Control command failed: ${response.status} ${response.statusText} for ${url}`, errorText);
      return false;
    }
  } catch (e) {
    console.error(`Control command error: ${e}`);
    return false;
  }
}
