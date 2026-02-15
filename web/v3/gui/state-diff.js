/**
 * State Diff View - Before/after comparison for state changes
 *
 * Shows which control changed, previous and new values,
 * and optionally the triggering event.
 */

import { jumpToEvent } from './event-timeline.js';

// ============================================================================
// State
// ============================================================================

let containerElement = null;
let currentEvent = null;

// ============================================================================
// Initialization
// ============================================================================

/**
 * Create the state diff view component
 * @param {HTMLElement} container - Container element
 */
export function createStateDiffView(container) {
  containerElement = container;
  containerElement.innerHTML = `
    <div class="debug-detail-header">
      <h3>State Change</h3>
      <span class="state-meta"></span>
    </div>
    <div class="debug-detail-content">
      <div class="state-placeholder">
        Select a state change event to view details
      </div>
    </div>
  `;
}

// ============================================================================
// Value Formatting
// ============================================================================

/**
 * Format a value for display
 */
function formatValue(value) {
  if (value === null || value === undefined) {
    return '—';
  }

  if (typeof value === 'boolean') {
    return value ? 'ON' : 'OFF';
  }

  if (typeof value === 'number') {
    // Format percentage values
    if (Number.isInteger(value) && value >= 0 && value <= 100) {
      return `${value}%`;
    }
    return String(value);
  }

  if (typeof value === 'string') {
    return value;
  }

  if (typeof value === 'object') {
    // Handle nested value objects
    if ('value' in value) {
      return formatValue(value.value);
    }

    // Handle mode objects
    if ('mode' in value) {
      return value.mode;
    }

    return JSON.stringify(value);
  }

  return String(value);
}

/**
 * Get a friendly name for a control ID
 */
function getControlDisplayName(controlId) {
  // Convert camelCase/snake_case to Title Case
  const name = controlId
    .replace(/_/g, ' ')
    .replace(/([A-Z])/g, ' $1')
    .trim()
    .toLowerCase()
    .replace(/\b\w/g, c => c.toUpperCase());

  return name;
}

/**
 * Determine the change type for styling
 */
function getChangeType(before, after) {
  const beforeNum = parseFloat(before);
  const afterNum = parseFloat(after);

  if (!isNaN(beforeNum) && !isNaN(afterNum)) {
    if (afterNum > beforeNum) return 'increase';
    if (afterNum < beforeNum) return 'decrease';
    return 'same';
  }

  // Boolean changes
  if (typeof before === 'boolean' || typeof after === 'boolean') {
    if (after === true || after === 'ON') return 'enabled';
    if (after === false || after === 'OFF') return 'disabled';
  }

  return 'changed';
}

/**
 * Escape HTML for safe insertion
 */
function escapeHtml(text) {
  const div = document.createElement('div');
  div.textContent = String(text);
  return div.innerHTML;
}

// ============================================================================
// Rendering
// ============================================================================

/**
 * Render the state diff view for an event
 */
function renderStateDiff(event) {
  const controlId = event.controlId || event.control_id || 'unknown';
  const before = event.before;
  const after = event.after;
  const triggerEventId = event.triggerEventId || event.trigger_event_id;

  const displayName = getControlDisplayName(controlId);
  const beforeStr = formatValue(before);
  const afterStr = formatValue(after);
  const changeType = getChangeType(before, after);

  let html = '<div class="state-diff">';

  // Main change card
  html += `
    <div class="state-diff-row">
      <div class="state-diff-control">${escapeHtml(displayName)}</div>
      <div class="state-diff-before">${escapeHtml(beforeStr)}</div>
      <div class="state-diff-arrow">→</div>
      <div class="state-diff-after">${escapeHtml(afterStr)}</div>
    </div>
  `;

  // Change description
  html += `
    <div style="padding: 8px; color: #888; font-size: 11px;">
      ${getChangeDescription(controlId, before, after, changeType)}
    </div>
  `;

  // Raw values for complex objects
  if (typeof before === 'object' || typeof after === 'object') {
    html += `
      <div style="margin-top: 12px; padding: 12px; background: #1d1d32; border-radius: 6px;">
        <div style="font-size: 10px; color: #666; margin-bottom: 8px;">RAW VALUES</div>
        <div style="display: flex; gap: 16px;">
          <div style="flex: 1;">
            <div style="font-size: 10px; color: #dd8888; margin-bottom: 4px;">Before:</div>
            <pre style="margin: 0; font-size: 10px; color: #aaa; overflow: auto;">${escapeHtml(JSON.stringify(before, null, 2))}</pre>
          </div>
          <div style="flex: 1;">
            <div style="font-size: 10px; color: #88ddcc; margin-bottom: 4px;">After:</div>
            <pre style="margin: 0; font-size: 10px; color: #aaa; overflow: auto;">${escapeHtml(JSON.stringify(after, null, 2))}</pre>
          </div>
        </div>
      </div>
    `;
  }

  // Trigger event link
  if (triggerEventId !== undefined && triggerEventId !== null) {
    html += `
      <div class="state-diff-trigger">
        Triggered by event: <a data-event-id="${triggerEventId}">#${triggerEventId}</a>
      </div>
    `;
  } else {
    html += `
      <div class="state-diff-trigger">
        <em>Trigger event not identified</em>
        <span style="margin-left: 8px; color: #666;">(may be from chart plotter)</span>
      </div>
    `;
  }

  html += '</div>';
  return html;
}

/**
 * Get a human-readable description of the change
 */
function getChangeDescription(controlId, before, after, changeType) {
  const displayName = getControlDisplayName(controlId);
  const beforeStr = formatValue(before);
  const afterStr = formatValue(after);

  switch (changeType) {
    case 'increase':
      return `${displayName} increased from ${beforeStr} to ${afterStr}`;
    case 'decrease':
      return `${displayName} decreased from ${beforeStr} to ${afterStr}`;
    case 'enabled':
      return `${displayName} was enabled`;
    case 'disabled':
      return `${displayName} was disabled`;
    case 'same':
      return `${displayName} value unchanged (${afterStr})`;
    default:
      return `${displayName} changed from "${beforeStr}" to "${afterStr}"`;
  }
}

// ============================================================================
// Public API
// ============================================================================

/**
 * Show state diff for an event
 * @param {Object} event - The state change event
 */
export function showStateDiff(event) {
  if (!containerElement) return;

  if (event.eventType !== 'stateChange') {
    // Not a state change event
    const content = containerElement.querySelector('.debug-detail-content');
    if (content) {
      content.innerHTML = `
        <div class="state-placeholder">
          This event is not a state change.
          <div style="margin-top: 8px; font-size: 11px; color: #666;">
            Event type: ${event.eventType || 'unknown'}
          </div>
        </div>
      `;
    }
    return;
  }

  currentEvent = event;

  const content = containerElement.querySelector('.debug-detail-content');
  const meta = containerElement.querySelector('.state-meta');

  if (!content) return;

  // Update meta info
  if (meta) {
    const radarId = event.radarId || event.radar_id || '';
    const timestamp = event.timestamp || 0;
    meta.textContent = `${radarId} | ${timestamp}ms`;
  }

  // Render the state diff
  content.innerHTML = renderStateDiff(event);

  // Set up click handler for trigger event link
  const triggerLink = content.querySelector('[data-event-id]');
  if (triggerLink) {
    triggerLink.addEventListener('click', (e) => {
      e.preventDefault();
      const eventId = parseInt(triggerLink.dataset.eventId);
      if (!isNaN(eventId)) {
        jumpToEvent(eventId);
      }
    });
  }
}

/**
 * Clear the state diff view
 */
export function clearStateDiff() {
  currentEvent = null;
  if (!containerElement) return;

  const content = containerElement.querySelector('.debug-detail-content');
  const meta = containerElement.querySelector('.state-meta');

  if (content) {
    content.innerHTML = `
      <div class="state-placeholder">
        Select a state change event to view details
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
