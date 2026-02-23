/**
 * Event Timeline - Scrollable list of debug events
 *
 * Displays protocol events with color-coded badges and filtering.
 */

// ============================================================================
// State
// ============================================================================

let containerElement = null;
let onEventClick = null;
let selectedEventId = null;
let autoScroll = true;

// ============================================================================
// Initialization
// ============================================================================

/**
 * Create the event timeline component
 * @param {HTMLElement} container - Container element
 * @param {Function} onClick - Callback when event is clicked
 */
export function createEventTimeline(container, onClick) {
  containerElement = container;
  onEventClick = onClick;

  containerElement.innerHTML = `
    <div class="debug-no-events">
      <p>No events yet</p>
      <p style="font-size: 10px; margin-top: 8px;">
        Waiting for radar traffic...
      </p>
    </div>
  `;

  // Detect when user scrolls manually
  containerElement.addEventListener('scroll', () => {
    const { scrollTop, scrollHeight, clientHeight } = containerElement;
    autoScroll = scrollHeight - scrollTop - clientHeight < 50;
  });
}

// ============================================================================
// Rendering
// ============================================================================

/**
 * Format timestamp as mm:ss.ms
 */
function formatTime(timestamp) {
  const ms = timestamp % 1000;
  const totalSeconds = Math.floor(timestamp / 1000);
  const seconds = totalSeconds % 60;
  const minutes = Math.floor(totalSeconds / 60);

  return `${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}.${ms.toString().padStart(3, '0')}`;
}

/**
 * Get badge info for event type
 */
function getBadgeInfo(event) {
  // Use eventType field (renamed from type to avoid conflict with WebSocket message type)
  if (event.eventType === 'data') {
    const isUnknown = event.decoded?.brand === 'unknown';
    if (isUnknown) {
      return { class: 'unknown', label: 'UNK' };
    }
    return event.direction === 'send'
      ? { class: 'send', label: 'SEND' }
      : { class: 'recv', label: 'RECV' };
  }

  if (event.eventType === 'socketOp') {
    return { class: 'socket', label: 'SOCK' };
  }

  if (event.eventType === 'stateChange') {
    return { class: 'state', label: 'STATE' };
  }

  return { class: 'unknown', label: '?' };
}

/**
 * Get summary text for an event
 */
function getEventSummary(event) {
  if (event.eventType === 'data') {
    const decoded = event.decoded;

    if (decoded && decoded.brand !== 'unknown') {
      // Use description if available
      if (decoded.description) {
        return decoded.description;
      }

      // Build summary from decoded info
      const msgType = decoded.messageType || decoded.message_type || '';
      const cmdId = decoded.commandId || decoded.command_id || '';

      if (cmdId) {
        return `<code>${cmdId}</code> ${msgType}`;
      }
      if (msgType) {
        return msgType;
      }
    }

    // Fallback to raw data preview
    const ascii = event.rawAscii || event.raw_ascii || '';
    if (ascii.length > 0) {
      const preview = ascii.substring(0, 40);
      return `<code>${escapeHtml(preview)}${ascii.length > 40 ? '...' : ''}</code>`;
    }

    return `${event.length || 0} bytes`;
  }

  if (event.eventType === 'socketOp') {
    const op = event.operation;
    if (!op) return 'Socket operation';

    switch (op.op) {
      case 'create':
        return `Create ${op.socketType || op.socket_type} socket`;
      case 'bind':
        return `Bind to port ${op.port}`;
      case 'connect':
        return `Connect to ${op.addr}:${op.port}`;
      case 'joinMulticast':
        return `Join multicast ${op.group}`;
      case 'setBroadcast':
        return `Broadcast ${op.enabled ? 'enabled' : 'disabled'}`;
      case 'close':
        return 'Close socket';
      default:
        return op.op;
    }
  }

  if (event.eventType === 'stateChange') {
    const controlId = event.controlId || event.control_id || 'unknown';
    const before = formatValue(event.before);
    const after = formatValue(event.after);
    return `${controlId}: ${before} → ${after}`;
  }

  return 'Unknown event';
}

/**
 * Format a control value for display
 */
function formatValue(value) {
  if (value === null || value === undefined) return '—';
  if (typeof value === 'object') return JSON.stringify(value);
  return String(value);
}

/**
 * Escape HTML for safe insertion
 */
function escapeHtml(text) {
  const div = document.createElement('div');
  div.textContent = text;
  return div.innerHTML;
}

/**
 * Render a single event element
 */
function renderEvent(event) {
  const el = document.createElement('div');
  el.className = 'debug-event';
  el.dataset.eventId = event.id;

  if (event.id === selectedEventId) {
    el.classList.add('selected');
  }

  const badge = getBadgeInfo(event);
  const summary = getEventSummary(event);
  const protocol = event.protocol ? event.protocol.toUpperCase() : '';
  const radarId = event.radarId || event.radar_id || '';

  el.innerHTML = `
    <div class="debug-event-header">
      <span class="debug-event-time">${formatTime(event.timestamp)}</span>
      <span class="debug-event-badge ${badge.class}">${badge.label}</span>
      ${protocol ? `<span class="debug-event-proto">${protocol}</span>` : ''}
      <span class="debug-event-radar">${radarId}</span>
    </div>
    <div class="debug-event-summary">${summary}</div>
  `;

  el.addEventListener('click', () => {
    if (onEventClick) {
      onEventClick(event.id);
    }
  });

  return el;
}

/**
 * Filter events based on criteria
 */
function filterEvents(events, filterText, filterRadar, filterType) {
  return events.filter(event => {
    // Filter by radar
    if (filterRadar !== 'all') {
      const radarId = event.radarId || event.radar_id;
      if (radarId !== filterRadar) return false;
    }

    // Filter by event type (using eventType field)
    if (filterType !== 'all') {
      if (filterType === 'unknown') {
        // Special filter: show only unknown/unparseable messages
        if (event.decoded?.brand !== 'unknown') return false;
      } else {
        if (event.eventType !== filterType) return false;
      }
    }

    // Filter by text
    if (filterText) {
      const text = filterText.toLowerCase();
      const searchables = [
        event.radarId || event.radar_id || '',
        event.brand || '',
        event.rawAscii || event.raw_ascii || '',
        event.decoded?.description || '',
        event.decoded?.commandId || event.decoded?.command_id || '',
        event.decoded?.messageType || event.decoded?.message_type || '',
        event.controlId || event.control_id || ''
      ].map(s => s.toLowerCase()).join(' ');

      if (!searchables.includes(text)) return false;
    }

    return true;
  });
}

// ============================================================================
// Public API
// ============================================================================

/**
 * Update the timeline with events
 * @param {Array} events - Array of events
 * @param {string} filterText - Text filter
 * @param {string} filterRadar - Radar ID filter
 * @param {string} filterType - Event type filter
 */
export function updateTimeline(events, filterText = '', filterRadar = 'all', filterType = 'all') {
  if (!containerElement) return;

  const filtered = filterEvents(events, filterText, filterRadar, filterType);

  if (filtered.length === 0) {
    containerElement.innerHTML = `
      <div class="debug-no-events">
        <p>${events.length === 0 ? 'No events yet' : 'No matching events'}</p>
        ${filterText || filterRadar !== 'all' || filterType !== 'all'
          ? '<p style="font-size: 10px; margin-top: 8px;">Try adjusting filters</p>'
          : '<p style="font-size: 10px; margin-top: 8px;">Waiting for radar traffic...</p>'
        }
      </div>
    `;
    return;
  }

  // Remember scroll position
  const wasAtBottom = autoScroll;

  // Render events
  containerElement.innerHTML = '';
  const fragment = document.createDocumentFragment();

  for (const event of filtered) {
    fragment.appendChild(renderEvent(event));
  }

  containerElement.appendChild(fragment);

  // Auto-scroll to bottom if was at bottom
  if (wasAtBottom) {
    containerElement.scrollTop = containerElement.scrollHeight;
  }
}

/**
 * Mark an event as selected
 * @param {number} eventId - Event ID to select
 */
export function selectEvent(eventId) {
  selectedEventId = eventId;

  // Update visual selection
  const events = containerElement?.querySelectorAll('.debug-event');
  events?.forEach(el => {
    el.classList.toggle('selected', parseInt(el.dataset.eventId) === eventId);
  });

  // Scroll selected event into view
  const selected = containerElement?.querySelector('.debug-event.selected');
  if (selected) {
    selected.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  }
}

/**
 * Clear all events
 */
export function clearEvents() {
  selectedEventId = null;
  if (containerElement) {
    containerElement.innerHTML = `
      <div class="debug-no-events">
        <p>No events</p>
        <p style="font-size: 10px; margin-top: 8px;">Events cleared</p>
      </div>
    `;
  }
}

/**
 * Jump to a specific event by ID
 * @param {number} eventId - Event ID to jump to
 */
export function jumpToEvent(eventId) {
  selectEvent(eventId);
  if (onEventClick) {
    onEventClick(eventId);
  }
}
