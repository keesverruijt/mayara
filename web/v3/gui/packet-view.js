/**
 * Packet View - Hex dump and decoded fields display
 *
 * Shows raw bytes as hex and ASCII, plus decoded protocol fields.
 */

// ============================================================================
// State
// ============================================================================

let containerElement = null;
let currentEvent = null;

// ============================================================================
// Initialization
// ============================================================================

/**
 * Create the packet view component
 * @param {HTMLElement} container - Container element
 */
export function createPacketView(container) {
  containerElement = container;
  containerElement.innerHTML = `
    <div class="debug-detail-header">
      <h3>Packet Details</h3>
      <span class="packet-meta"></span>
    </div>
    <div class="debug-detail-content">
      <div class="packet-placeholder">
        Select an event to view details
      </div>
    </div>
  `;
}

// ============================================================================
// Hex Dump Rendering
// ============================================================================

/**
 * Convert hex string to byte array
 */
function hexToBytes(hexString) {
  if (!hexString) return [];
  return hexString.split(' ').map(b => parseInt(b, 16));
}

/**
 * Render hex dump with 16 bytes per row
 */
function renderHexDump(hexString, asciiString) {
  const bytes = hexToBytes(hexString);
  if (bytes.length === 0) {
    return '<div class="hex-dump"><em>No data</em></div>';
  }

  let html = '<div class="hex-dump">';

  for (let offset = 0; offset < bytes.length; offset += 16) {
    const rowBytes = bytes.slice(offset, offset + 16);
    const rowAscii = asciiString ? asciiString.substring(offset, offset + 16) : '';

    // Format offset
    const offsetStr = offset.toString(16).padStart(4, '0');

    // Format hex bytes with spacing every 8 bytes
    const hexParts = [];
    for (let i = 0; i < rowBytes.length; i++) {
      hexParts.push(rowBytes[i].toString(16).padStart(2, '0'));
      if (i === 7) hexParts.push('');
    }
    const hexStr = hexParts.join(' ').padEnd(49, ' ');

    // Format ASCII
    const asciiStr = rowAscii.padEnd(16, ' ');

    html += `
      <div class="hex-row">
        <span class="hex-offset">${offsetStr}</span>
        <span class="hex-bytes">${hexStr}</span>
        <span class="hex-ascii">${escapeHtml(asciiStr)}</span>
      </div>
    `;
  }

  html += '</div>';
  return html;
}

/**
 * Escape HTML for safe insertion
 */
function escapeHtml(text) {
  const div = document.createElement('div');
  div.textContent = text;
  return div.innerHTML;
}

// ============================================================================
// Decoded Fields Rendering
// ============================================================================

/**
 * Render decoded message fields
 */
function renderDecodedFields(decoded) {
  if (!decoded) {
    return '';
  }

  if (decoded.brand === 'unknown') {
    return `
      <div class="decoded-section">
        <h4>Decoding Failed</h4>
        <div class="decoded-description" style="color: #ff8866;">
          ${escapeHtml(decoded.reason || 'Unknown format')}
        </div>
        ${decoded.partial ? `
          <div class="decoded-field">
            <span class="decoded-field-name">Partial data:</span>
            <span class="decoded-field-value">${formatJson(decoded.partial)}</span>
          </div>
        ` : ''}
      </div>
    `;
  }

  const brand = decoded.brand || 'unknown';
  const msgType = decoded.messageType || decoded.message_type || '';
  const cmdId = decoded.commandId || decoded.command_id || '';
  const fields = decoded.fields || {};
  const description = decoded.description || '';

  let html = `<div class="decoded-section">`;
  html += `<h4>Decoded (${brand})</h4>`;

  // Show command info
  if (cmdId) {
    html += `
      <div class="decoded-field">
        <span class="decoded-field-name">Command:</span>
        <span class="decoded-field-value">${escapeHtml(cmdId)}</span>
      </div>
    `;
  }

  if (msgType) {
    html += `
      <div class="decoded-field">
        <span class="decoded-field-name">Type:</span>
        <span class="decoded-field-value">${escapeHtml(msgType)}</span>
      </div>
    `;
  }

  // Show fields
  if (typeof fields === 'object' && Object.keys(fields).length > 0) {
    for (const [key, value] of Object.entries(fields)) {
      html += `
        <div class="decoded-field">
          <span class="decoded-field-name">${escapeHtml(key)}:</span>
          <span class="decoded-field-value">${formatFieldValue(value)}</span>
        </div>
      `;
    }
  }

  // Show description
  if (description) {
    html += `
      <div class="decoded-description">
        ${escapeHtml(description)}
      </div>
    `;
  }

  html += '</div>';
  return html;
}

/**
 * Format a field value for display
 */
function formatFieldValue(value) {
  if (value === null || value === undefined) {
    return '<em>null</em>';
  }

  if (typeof value === 'boolean') {
    return value ? '<span style="color: #88dd88">true</span>' : '<span style="color: #dd8888">false</span>';
  }

  if (typeof value === 'number') {
    return `<span style="color: #88aaff">${value}</span>`;
  }

  if (typeof value === 'string') {
    return escapeHtml(value);
  }

  if (Array.isArray(value)) {
    if (value.length > 10) {
      return `[${value.length} items]`;
    }
    return escapeHtml(JSON.stringify(value));
  }

  if (typeof value === 'object') {
    return formatJson(value);
  }

  return escapeHtml(String(value));
}

/**
 * Format JSON object compactly
 */
function formatJson(obj) {
  try {
    const str = JSON.stringify(obj, null, 2);
    if (str.length > 200) {
      return `<pre style="margin: 0; font-size: 10px; overflow: auto;">${escapeHtml(str)}</pre>`;
    }
    return `<code>${escapeHtml(str)}</code>`;
  } catch (e) {
    return '<em>Invalid JSON</em>';
  }
}

// ============================================================================
// Socket Operation Rendering
// ============================================================================

/**
 * Render socket operation details
 */
function renderSocketOp(event) {
  const op = event.operation;
  if (!op) {
    return '<div class="decoded-section"><em>No operation data</em></div>';
  }

  let html = '<div class="decoded-section">';
  html += '<h4>Socket Operation</h4>';

  html += `
    <div class="decoded-field">
      <span class="decoded-field-name">Operation:</span>
      <span class="decoded-field-value">${escapeHtml(op.op || 'unknown')}</span>
    </div>
  `;

  if (op.socketType || op.socket_type) {
    html += `
      <div class="decoded-field">
        <span class="decoded-field-name">Socket Type:</span>
        <span class="decoded-field-value">${(op.socketType || op.socket_type || '').toUpperCase()}</span>
      </div>
    `;
  }

  if (op.port !== undefined) {
    html += `
      <div class="decoded-field">
        <span class="decoded-field-name">Port:</span>
        <span class="decoded-field-value">${op.port}</span>
      </div>
    `;
  }

  if (op.addr) {
    html += `
      <div class="decoded-field">
        <span class="decoded-field-name">Address:</span>
        <span class="decoded-field-value">${escapeHtml(op.addr)}</span>
      </div>
    `;
  }

  if (op.group) {
    html += `
      <div class="decoded-field">
        <span class="decoded-field-name">Multicast Group:</span>
        <span class="decoded-field-value">${escapeHtml(op.group)}</span>
      </div>
    `;
  }

  if (op.interface) {
    html += `
      <div class="decoded-field">
        <span class="decoded-field-name">Interface:</span>
        <span class="decoded-field-value">${escapeHtml(op.interface)}</span>
      </div>
    `;
  }

  if (event.success !== undefined) {
    html += `
      <div class="decoded-field">
        <span class="decoded-field-name">Result:</span>
        <span class="decoded-field-value">${event.success
          ? '<span style="color: #88dd88">Success</span>'
          : '<span style="color: #dd8888">Failed</span>'
        }</span>
      </div>
    `;
  }

  if (event.error) {
    html += `
      <div class="decoded-description" style="color: #ff8866;">
        Error: ${escapeHtml(event.error)}
      </div>
    `;
  }

  html += '</div>';
  return html;
}

// ============================================================================
// Public API
// ============================================================================

/**
 * Show packet details for an event
 * @param {Object} event - The debug event
 */
export function showPacketDetails(event) {
  if (!containerElement) return;
  currentEvent = event;

  const content = containerElement.querySelector('.debug-detail-content');
  const meta = containerElement.querySelector('.packet-meta');

  if (!content) return;

  // Update meta info
  if (meta) {
    const radarId = event.radarId || event.radar_id || '';
    const brand = event.brand || '';
    const timestamp = event.timestamp || 0;
    meta.textContent = `${brand} | ${radarId} | ${timestamp}ms`;
  }

  // Handle different event types (using eventType field)
  if (event.eventType === 'data') {
    const hexString = event.rawHex || event.raw_hex || '';
    const asciiString = event.rawAscii || event.raw_ascii || '';
    const decoded = event.decoded;

    const direction = event.direction === 'send' ? '→ SEND' : '← RECV';
    const protocol = (event.protocol || 'udp').toUpperCase();
    const remoteAddr = event.remoteAddr || event.remote_addr || '';
    const remotePort = event.remotePort || event.remote_port || '';
    const length = event.length || 0;

    content.innerHTML = `
      <div style="margin-bottom: 12px; color: #888;">
        <strong>${direction}</strong> ${protocol} ${remoteAddr}:${remotePort}
        <span style="float: right;">${length} bytes</span>
      </div>
      ${renderHexDump(hexString, asciiString)}
      ${renderDecodedFields(decoded)}
    `;
  } else if (event.eventType === 'socketOp') {
    content.innerHTML = renderSocketOp(event);
  } else {
    content.innerHTML = `
      <div class="packet-placeholder">
        Event type: ${event.eventType || 'unknown'}
        <pre style="margin-top: 12px; font-size: 10px;">${escapeHtml(JSON.stringify(event, null, 2))}</pre>
      </div>
    `;
  }
}

/**
 * Clear the packet view
 */
export function clearPacketView() {
  currentEvent = null;
  if (!containerElement) return;

  const content = containerElement.querySelector('.debug-detail-content');
  const meta = containerElement.querySelector('.packet-meta');

  if (content) {
    content.innerHTML = `
      <div class="packet-placeholder">
        Select an event to view details
      </div>
    `;
  }

  if (meta) {
    meta.textContent = '';
  }
}

/**
 * Get the currently displayed event
 */
export function getCurrentEvent() {
  return currentEvent;
}
