export { PPI };

import { SpokeProcessorFactory } from "./spoke_processor.js";

// Factor by which we fill the (w,h) canvas with the outer radar range ring
const RANGE_SCALE = 0.9;

const NAUTICAL_MILE = 1852.0;

function divides_near(a, b) {
  let remainder = a % b;
  return remainder <= 1.0 || remainder >= b - 1;
}

function is_metric(v) {
  if (v <= 100) {
    return divides_near(v, 25);
  } else if (v <= 750) {
    return divides_near(v, 50);
  }
  return divides_near(v, 500);
}

function formatRangeValue(metric, v) {
  if (metric) {
    v = Math.round(v);
    if (v >= 1000) {
      return v / 1000 + " km";
    } else {
      return v + " m";
    }
  } else {
    if (v >= NAUTICAL_MILE - 1) {
      if (divides_near(v, NAUTICAL_MILE)) {
        return Math.floor((v + 1) / NAUTICAL_MILE) + " nm";
      } else {
        return v / NAUTICAL_MILE + " nm";
      }
    } else if (divides_near(v, NAUTICAL_MILE / 2)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 2)) + "/2 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 4)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 4)) + "/4 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 8)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 8)) + "/8 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 16)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 16)) + "/16 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 32)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 32)) + "/32 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 64)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 64)) + "/64 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 128)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 128)) + "/128 nm";
    } else {
      return v / NAUTICAL_MILE + " nm";
    }
  }
}

/**
 * PPI (Plan Position Indicator) - Manages the radar display overlay and spoke processing
 * This class is renderer-agnostic and can work with WebGPU, WebGL, or Canvas2D backends
 */
class PPI {
  /**
   * @param {object} renderer - Backend renderer (WebGPU, WebGL, etc.) with renderSpoke/render methods
   * @param {HTMLCanvasElement} overlayCanvas - Canvas element for overlay graphics
   * @param {HTMLCanvasElement} backgroundCanvas - Canvas element for background graphics
   */
  constructor(renderer, overlayCanvas, backgroundCanvas) {
    this.renderer = renderer;
    this.overlay_dom = overlayCanvas;
    this.overlay_ctx = overlayCanvas ? overlayCanvas.getContext("2d") : null;
    this.background_dom = backgroundCanvas;
    this.background_ctx = backgroundCanvas ? backgroundCanvas.getContext("2d") : null;

    // Display dimensions
    this.width = 0;
    this.height = 0;
    this.center_x = 0;
    this.center_y = 0;
    this.beam_length = 0;

    // Radar state
    this.range = 0;
    this.actual_range = 0;
    this.lastHeading = null;
    this.headingRotation = 0;
    this.headingMode = "headingUp";
    this.trueHeading = 0;

    // Spoke data
    this.data = null;
    this.spokesPerRevolution = 0;
    this.maxspokelength = 0;
    this.legend = null;

    // Spoke processing strategy
    this.spokeProcessor = null;
    this.processingMode = "auto"; // "auto", "clean", or "smoothing"

    // Buffer flush - wait for full rotation after standby/range change
    this.waitForRotation = false;
    this.waitStartAngle = -1;
    this.seenAngleWrap = false;
    this.lastWaitAngle = 0;

    // Power mode state
    this.powerMode = "off";
    this.onTimeSeconds = 0;
    this.txTimeSeconds = 0;

    // Guard zones and no-transmit sectors
    this.guardZones = [null, null];
    this.noTransmitSectors = [null, null, null, null];

    // Zone edit mode state
    this.editingZoneIndex = null;
    this.dragState = null;
    this.hoveredHandle = null;
    this.onZoneDragEnd = null;

    // Sector edit mode state
    this.editingSectorIndex = null;
    this.onSectorDragEnd = null;

    // Drag handlers bound state
    this._dragHandlersInstalled = false;
  }

  /**
   * Initialize spoke data buffers
   */
  setSpokes(spokesPerRevolution, maxspokelength) {
    this.spokesPerRevolution = spokesPerRevolution;
    this.maxspokelength = maxspokelength;
    this.data = new Uint8Array(spokesPerRevolution * maxspokelength);

    // Create spoke processor if legend is available
    if (this.legend) {
      this.#createSpokeProcessor();
    }
  }

  setRange(range) {
    this.range = range;
    if (this.data) {
      this.data.fill(0);
    }
    this.redrawCanvas();
  }

  setHeadingMode(mode) {
    if (this.lastHeading || this.trueHeading) {
      this.headingMode = mode;
      return mode;
    }
    return "headingUp";
  }

  setTrueHeading(heading) {
    this.trueHeading = heading;
  }

  getTrueHeading() {
    return this.trueHeading;
  }

  getHeadingMode() {
    return this.headingMode;
  }

  setPowerMode(powerMode, onTimeSeconds, txTimeSeconds) {
    const isStandby = powerMode !== "transmit";
    const wasStandby = this.powerMode !== "transmit";
    this.powerMode = powerMode;
    this.onTimeSeconds = onTimeSeconds || 0;
    this.txTimeSeconds = txTimeSeconds || 0;

    if (isStandby && !wasStandby) {
      this.clearRadarDisplay();
    } else if (!isStandby && wasStandby) {
      this.clearRadarDisplay();
    }

    this.redrawCanvas();
  }

  clearRadarDisplay() {
    if (this.data) {
      this.data.fill(0);
    }
    if (this.spokeProcessor) {
      this.spokeProcessor.reset();
    }

    if (this.spokeProcessor && this.spokeProcessor.needsRotationWait()) {
      this.waitForRotation = true;
      this.waitStartAngle = -1;
      this.seenAngleWrap = false;
    } else {
      this.waitForRotation = false;
    }

    // Tell renderer to clear its display
    if (this.renderer && this.renderer.clearDisplay) {
      this.renderer.clearDisplay(this.data, this.spokesPerRevolution, this.maxspokelength);
    }
  }

  setProcessingMode(mode) {
    if (this.processingMode !== mode) {
      this.processingMode = mode;
      this.#createSpokeProcessor();
      this.clearRadarDisplay();
    }
  }

  setGuardZone(index, zone) {
    if (index >= 0 && index < 2) {
      this.guardZones[index] = zone;
      this.redrawCanvas();
    }
  }

  setNoTransmitSector(index, sector) {
    if (index >= 0 && index < 4) {
      this.noTransmitSectors[index] = sector;
      this.redrawCanvas();
    }
  }

  setEditingZone(index, onDragEnd = null) {
    this.editingZoneIndex = index;
    this.onZoneDragEnd = onDragEnd;
    this.dragState = null;
    this.hoveredHandle = null;

    this.#updatePointerEvents();
    this.redrawCanvas();
  }

  setEditingSector(index, onDragEnd = null) {
    this.editingSectorIndex = index;
    this.onSectorDragEnd = onDragEnd;
    this.dragState = null;
    this.hoveredHandle = null;

    this.#updatePointerEvents();
    this.redrawCanvas();
  }

  setLegend(legend) {
    this.legend = this.#convertServerLegend(legend);

    // Create spoke processor now that we have legend
    if (this.spokesPerRevolution) {
      this.#createSpokeProcessor();
    }

    // Pass legend to renderer for color table
    if (this.renderer && this.renderer.setLegend) {
      this.renderer.setLegend(this.legend);
    }
  }

  #convertServerLegend(serverLegend) {
    const colors = new Array(256);

    for (let i = 0; i < 256; i++) {
      colors[i] = [255, 0, 0, 255];
    }

    for (let i = 0; i < serverLegend.pixels.length && i < 256; i++) {
      const entry = serverLegend.pixels[i];
      if (entry.color) {
        colors[i] = this.#hexToRGBA(entry.color);
      }
    }

    return {
      colors: colors,
      lowReturn: serverLegend.lowReturn,
      mediumReturn: serverLegend.mediumReturn,
      strongReturn: serverLegend.strongReturn,
      specialStart: serverLegend.pixelColors,
    };
  }

  #hexToRGBA(hex) {
    let a = [];
    for (let i = 1; i < hex.length; i += 2) {
      a.push(parseInt(hex.slice(i, i + 2), 16));
    }
    while (a.length < 3) {
      a.push(0);
    }
    while (a.length < 4) {
      a.push(255);
    }
    return a;
  }

  #createSpokeProcessor() {
    if (!this.legend || !this.spokesPerRevolution) {
      return;
    }
    this.spokeProcessor = SpokeProcessorFactory.create(
      this.processingMode,
      this.spokesPerRevolution,
      this.legend
    );
  }

  /**
   * Process and draw a spoke
   */
  drawSpoke(spoke) {
    if (!this.data || !this.legend || !this.spokeProcessor) return;

    // Extract heading from spoke if available
    if (spoke.bearing && spoke.angle) {
      const heading =
        (spoke.bearing + this.spokesPerRevolution - spoke.angle) %
        this.spokesPerRevolution;
      this.lastHeading = (spoke.heading * 360) / this.spokesPerRevolution;
    } else {
      this.lastHeading = null;
    }

    // Don't draw spokes in standby mode
    if (this.powerMode !== "transmit") {
      if (this.spokeProcessor.needsRotationWait()) {
        this.waitForRotation = true;
        this.waitStartAngle = -1;
        this.seenAngleWrap = false;
      }
      return;
    }

    if (spoke.angle >= this.spokesPerRevolution) {
      console.error(`Bad spoke angle: ${spoke.angle} >= ${this.spokesPerRevolution}`);
      return;
    }

    // Wait for full rotation
    if (this.waitForRotation) {
      if (this.waitStartAngle < 0) {
        this.waitStartAngle = spoke.angle;
        this.lastWaitAngle = spoke.angle;
        return;
      }

      if (
        !this.seenAngleWrap &&
        spoke.angle < this.lastWaitAngle - this.spokesPerRevolution / 2
      ) {
        this.seenAngleWrap = true;
      }

      if (this.seenAngleWrap && spoke.angle >= this.waitStartAngle) {
        this.waitForRotation = false;
        if (this.spokeProcessor) {
          this.spokeProcessor.reset();
        }
        if (this.data) this.data.fill(0);
        if (this.renderer && this.renderer.clearDisplay) {
          this.renderer.clearDisplay(this.data, this.spokesPerRevolution, this.maxspokelength);
        }
      } else {
        this.lastWaitAngle = spoke.angle;
        return;
      }
    }

    // Handle range changes
    if (this.actual_range !== spoke.range) {
      const wasInitialRange = this.actual_range === 0;
      this.actual_range = spoke.range;
      this.data.fill(0);
      if (this.spokeProcessor) {
        this.spokeProcessor.reset();
      }
      this.redrawCanvas();

      if (!wasInitialRange && this.spokeProcessor.needsRotationWait()) {
        this.waitForRotation = true;
        this.waitStartAngle = -1;
        this.seenAngleWrap = false;
        if (this.renderer && this.renderer.clearDisplay) {
          this.renderer.clearDisplay(this.data, this.spokesPerRevolution, this.maxspokelength);
        }
        return;
      }
    }

    // Update rotation tracking
    this.spokeProcessor.updateRotationTracking(spoke.angle, this.spokesPerRevolution);

    // Process spoke using current strategy
    this.spokeProcessor.processSpoke(
      this.data,
      spoke,
      this.spokesPerRevolution,
      this.maxspokelength
    );
  }

  /**
   * Render the current spoke data to the display
   */
  render() {
    if (this.renderer && this.renderer.render) {
      this.renderer.render(this.data, this.spokesPerRevolution, this.maxspokelength);
    }
  }

  /**
   * Resize and redraw the canvas
   */
  redrawCanvas() {
    const parent = this.overlay_dom?.parentNode;
    if (!parent) return;

    const styles = getComputedStyle(parent);
    const w = parseInt(styles.getPropertyValue("width"), 10);
    const h = parseInt(styles.getPropertyValue("height"), 10);

    if (this.overlay_dom) {
      this.overlay_dom.width = w;
      this.overlay_dom.height = h;
    }
    if (this.background_dom) {
      this.background_dom.width = w;
      this.background_dom.height = h;
    }

    this.width = w;
    this.height = h;
    this.center_x = w / 2;
    this.center_y = h / 2;
    this.beam_length = Math.trunc(Math.max(this.center_x, this.center_y) * RANGE_SCALE);

    // Update heading rotation
    let trueHeadingDeg = this.lastHeading;
    if (!trueHeadingDeg && this.trueHeading) {
      trueHeadingDeg = (this.trueHeading * 180) / Math.PI;
    }
    if (trueHeadingDeg && this.headingMode === "northUp") {
      this.headingRotation = (trueHeadingDeg * Math.PI) / 180;
    } else {
      this.headingRotation = 0;
    }

    // Draw overlay
    this.#drawOverlay();

    // Notify renderer of resize
    if (this.renderer && this.renderer.resize) {
      this.renderer.resize(w, h, this.beam_length, this.headingRotation);
    }
  }

  // ============================================================
  // Overlay drawing
  // ============================================================

  #drawOverlay() {
    if (!this.overlay_ctx) return;

    const ctx = this.overlay_ctx;
    const range = this.range || this.actual_range;

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, this.width, this.height);

    // Draw no-transmit sectors
    for (const sector of this.noTransmitSectors) {
      this.#drawNoTransmitSector(ctx, sector, "rgba(255, 255, 200, 0.25)", "rgba(200, 200, 0, 0.6)");
    }

    // Draw guard zones
    this.#drawGuardZone(ctx, this.guardZones[0], "rgba(144, 238, 144, 0.25)", "rgba(0, 128, 0, 0.6)");
    this.#drawGuardZone(ctx, this.guardZones[1], "rgba(173, 216, 230, 0.25)", "rgba(0, 0, 255, 0.6)");

    // Draw drag handles if editing
    if (this.editingZoneIndex !== null) {
      const zone = this.guardZones[this.editingZoneIndex];
      this.#drawDragHandles(ctx, zone);
    }
    if (this.editingSectorIndex !== null) {
      const sector = this.noTransmitSectors[this.editingSectorIndex];
      this.#drawSectorDragHandles(ctx, sector);
    }

    // Draw standby overlay
    if (this.powerMode !== "transmit") {
      this.#drawStandbyOverlay(ctx);
    }

    // Draw range rings
    this.#drawRangeRings(ctx, range);

    // Draw compass rose
    this.#drawCompassRose(ctx);
  }

  #drawStandbyOverlay(ctx) {
    ctx.save();

    ctx.fillStyle = "white";
    ctx.font = "bold 36px/1 Verdana, Geneva, sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";

    ctx.shadowColor = "black";
    ctx.shadowBlur = 4;
    ctx.shadowOffsetX = 2;
    ctx.shadowOffsetY = 2;

    const standbyY =
      this.onTimeSeconds > 0 || this.txTimeSeconds > 0
        ? this.center_y - 40
        : this.center_y;

    ctx.fillText(this.powerMode.toUpperCase(), this.center_x, standbyY);

    ctx.font = "bold 20px/1 Verdana, Geneva, sans-serif";
    let yOffset = this.center_y + 10;

    if (this.onTimeSeconds > 0) {
      const onTimeStr = this.#formatSecondsAsTimeZero(this.onTimeSeconds);
      ctx.fillText("ON-TIME: " + onTimeStr, this.center_x, yOffset);
      yOffset += 30;
    }

    if (this.txTimeSeconds > 0) {
      const txTimeStr = this.#formatSecondsAsTimeZero(this.txTimeSeconds);
      ctx.fillText("TX-TIME: " + txTimeStr, this.center_x, yOffset);
    }

    ctx.restore();
  }

  #formatSecondsAsTimeZero(totalSeconds) {
    totalSeconds = Math.floor(totalSeconds);
    const days = Math.floor(totalSeconds / 86400);
    const remainingAfterDays = totalSeconds % 86400;
    const hours = Math.floor(remainingAfterDays / 3600);
    const minutes = Math.floor((remainingAfterDays % 3600) / 60);
    const seconds = remainingAfterDays % 60;

    const hh = hours.toString().padStart(2, "0");
    const mm = minutes.toString().padStart(2, "0");
    const ss = seconds.toString().padStart(2, "0");

    return `${days}.${hh}:${mm}:${ss}`;
  }

  #drawNoTransmitSector(ctx, sector, fillColor, strokeColor) {
    if (!sector) return;

    const radius = this.beam_length * 3;
    if (radius <= 0) return;

    const startAngle = sector.startAngle - Math.PI / 2;
    const endAngle = sector.endAngle - Math.PI / 2;
    const isCircle = Math.abs(sector.endAngle - sector.startAngle) < 0.001;

    ctx.beginPath();
    if (isCircle) {
      ctx.arc(this.center_x, this.center_y, radius, 0, 2 * Math.PI);
    } else {
      ctx.moveTo(this.center_x, this.center_y);
      ctx.arc(this.center_x, this.center_y, radius, startAngle, endAngle);
      ctx.closePath();
    }

    ctx.fillStyle = fillColor;
    ctx.fill();
    ctx.strokeStyle = strokeColor;
    ctx.lineWidth = 1;
    ctx.stroke();
  }

  #drawGuardZone(ctx, zone, fillColor, strokeColor) {
    if (!zone) return;

    const range = this.range || this.actual_range;
    if (!range || range <= 0) return;

    const pixelsPerMeter = this.beam_length / range;
    const innerRadius = zone.startDistance * pixelsPerMeter;
    const outerRadius = zone.endDistance * pixelsPerMeter;

    if (outerRadius <= 0) return;

    const startAngle = zone.startAngle - Math.PI / 2;
    const endAngle = zone.endAngle - Math.PI / 2;
    const isCircle = Math.abs(zone.endAngle - zone.startAngle) < 0.001;

    ctx.beginPath();
    if (isCircle) {
      ctx.arc(this.center_x, this.center_y, outerRadius, 0, 2 * Math.PI);
      if (innerRadius > 0) {
        ctx.moveTo(this.center_x + innerRadius, this.center_y);
        ctx.arc(this.center_x, this.center_y, innerRadius, 0, 2 * Math.PI, true);
      }
    } else {
      ctx.arc(this.center_x, this.center_y, outerRadius, startAngle, endAngle);
      if (innerRadius > 0) {
        ctx.arc(this.center_x, this.center_y, innerRadius, endAngle, startAngle, true);
      } else {
        ctx.lineTo(this.center_x, this.center_y);
      }
      ctx.closePath();
    }

    ctx.fillStyle = fillColor;
    ctx.fill();
    ctx.strokeStyle = strokeColor;
    ctx.lineWidth = 1;
    ctx.stroke();
  }

  #drawRangeRings(ctx, range) {
    ctx.strokeStyle = "#00ff00";
    ctx.lineWidth = 1.5;
    ctx.fillStyle = "#00ff00";
    ctx.font = "bold 14px/1 Verdana, Geneva, sans-serif";

    for (let i = 1; i <= 4; i++) {
      const radius = (i * this.beam_length) / 4;
      ctx.beginPath();
      ctx.arc(this.center_x, this.center_y, radius, 0, 2 * Math.PI);
      ctx.stroke();

      if (range) {
        const text = formatRangeValue(is_metric(range), (range * i) / 4);
        const labelX = this.center_x + radius * 0.707;
        const labelY = this.center_y - radius * 0.707;
        ctx.fillText(text, labelX + 5, labelY - 5);
      }
    }
  }

  #drawCompassRose(ctx) {
    const degreeRingRadius = (3 * this.beam_length) / 4;
    const tickLength = 8;
    const majorTickLength = 12;

    ctx.font = "bold 12px/1 Verdana, Geneva, sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.strokeStyle = "#00ff00";
    ctx.fillStyle = "#00ff00";

    let trueHeadingDeg = this.lastHeading;
    if (!trueHeadingDeg && this.trueHeading) {
      trueHeadingDeg = (this.trueHeading * 180) / Math.PI;
    }
    if (!trueHeadingDeg) {
      trueHeadingDeg = 0;
    }

    const roseRotationDeg = this.headingMode === "headingUp" ? -trueHeadingDeg : 0;

    for (let deg = 0; deg < 360; deg += 10) {
      const displayDeg = deg + roseRotationDeg;
      const radians = ((90 - displayDeg) * Math.PI) / 180;

      const cos = Math.cos(radians);
      const sin = Math.sin(radians);

      const isMajor = deg % 30 === 0;
      const tick = isMajor ? majorTickLength : tickLength;

      const innerRadius = degreeRingRadius - tick / 2;
      const outerRadius = degreeRingRadius + tick / 2;

      const x1 = this.center_x + innerRadius * cos;
      const y1 = this.center_y - innerRadius * sin;
      const x2 = this.center_x + outerRadius * cos;
      const y2 = this.center_y - outerRadius * sin;

      ctx.beginPath();
      ctx.moveTo(x1, y1);
      ctx.lineTo(x2, y2);
      ctx.stroke();

      if (isMajor) {
        const labelRadius = degreeRingRadius + majorTickLength + 10;
        const labelX = this.center_x + labelRadius * cos;
        const labelY = this.center_y - labelRadius * sin;
        ctx.fillText(deg.toString(), labelX, labelY);
      }
    }

    // Draw North indicator
    const northDeg = roseRotationDeg;
    const northRadians = ((90 - northDeg) * Math.PI) / 180;
    const northRadius = degreeRingRadius + majorTickLength + 25;
    const northX = this.center_x + northRadius * Math.cos(northRadians);
    const northY = this.center_y - northRadius * Math.sin(northRadians);
    ctx.font = "bold 14px/1 Verdana, Geneva, sans-serif";
    ctx.fillText("N", northX, northY);
  }

  // ============================================================
  // Drag handles for zone/sector editing
  // ============================================================

  #updatePointerEvents() {
    const isEditing = this.editingZoneIndex !== null || this.editingSectorIndex !== null;

    if (isEditing && this.overlay_dom) {
      this.overlay_dom.style.pointerEvents = "auto";
      this.overlay_dom.style.cursor = "default";
      this.#setupDragHandlers();
    } else if (this.overlay_dom) {
      this.overlay_dom.style.pointerEvents = "none";
      this.overlay_dom.style.cursor = "default";
      this.#removeDragHandlers();
    }
  }

  #setupDragHandlers() {
    if (this._dragHandlersInstalled) return;
    this._dragHandlersInstalled = true;

    this._onMouseDown = this.#handleMouseDown.bind(this);
    this._onMouseMove = this.#handleMouseMove.bind(this);
    this._onMouseUp = this.#handleMouseUp.bind(this);
    this._onTouchStart = this.#handleTouchStart.bind(this);
    this._onTouchMove = this.#handleTouchMove.bind(this);
    this._onTouchEnd = this.#handleTouchEnd.bind(this);

    this.overlay_dom.addEventListener("mousedown", this._onMouseDown);
    this.overlay_dom.addEventListener("mousemove", this._onMouseMove);
    this.overlay_dom.addEventListener("mouseup", this._onMouseUp);
    this.overlay_dom.addEventListener("mouseleave", this._onMouseUp);
    this.overlay_dom.addEventListener("touchstart", this._onTouchStart);
    this.overlay_dom.addEventListener("touchmove", this._onTouchMove);
    this.overlay_dom.addEventListener("touchend", this._onTouchEnd);
  }

  #removeDragHandlers() {
    if (!this._dragHandlersInstalled) return;
    this._dragHandlersInstalled = false;

    this.overlay_dom.removeEventListener("mousedown", this._onMouseDown);
    this.overlay_dom.removeEventListener("mousemove", this._onMouseMove);
    this.overlay_dom.removeEventListener("mouseup", this._onMouseUp);
    this.overlay_dom.removeEventListener("mouseleave", this._onMouseUp);
    this.overlay_dom.removeEventListener("touchstart", this._onTouchStart);
    this.overlay_dom.removeEventListener("touchmove", this._onTouchMove);
    this.overlay_dom.removeEventListener("touchend", this._onTouchEnd);
  }

  #getCanvasCoords(event) {
    const rect = this.overlay_dom.getBoundingClientRect();
    return {
      x: event.clientX - rect.left,
      y: event.clientY - rect.top,
    };
  }

  #pixelToRadarCoords(x, y) {
    const dx = x - this.center_x;
    const dy = y - this.center_y;
    const angle = Math.atan2(dx, -dy);
    const pixelDist = Math.sqrt(dx * dx + dy * dy);
    const range = this.range || this.actual_range;
    const pixelsPerMeter = range > 0 ? this.beam_length / range : 1;
    const distance = pixelDist / pixelsPerMeter;
    return { angle, distance };
  }

  #getHandlePositions(zone) {
    if (!zone) return null;

    const range = this.range || this.actual_range;
    if (!range || range <= 0) return null;

    const pixelsPerMeter = this.beam_length / range;
    const innerRadius = zone.startDistance * pixelsPerMeter;
    const outerRadius = zone.endDistance * pixelsPerMeter;
    const midRadius = (innerRadius + outerRadius) / 2;

    let midAngle = (zone.startAngle + zone.endAngle) / 2;
    if (zone.endAngle < zone.startAngle) {
      midAngle = (zone.startAngle + zone.endAngle + 2 * Math.PI) / 2;
      if (midAngle > Math.PI) midAngle -= 2 * Math.PI;
    }

    const startAngleX = this.center_x + midRadius * Math.sin(zone.startAngle);
    const startAngleY = this.center_y - midRadius * Math.cos(zone.startAngle);
    const endAngleX = this.center_x + midRadius * Math.sin(zone.endAngle);
    const endAngleY = this.center_y - midRadius * Math.cos(zone.endAngle);

    const innerDistX = this.center_x + innerRadius * Math.sin(midAngle);
    const innerDistY = this.center_y - innerRadius * Math.cos(midAngle);
    const outerDistX = this.center_x + outerRadius * Math.sin(midAngle);
    const outerDistY = this.center_y - outerRadius * Math.cos(midAngle);

    return {
      startAngle: { x: startAngleX, y: startAngleY, angle: zone.startAngle },
      endAngle: { x: endAngleX, y: endAngleY, angle: zone.endAngle },
      innerDist: { x: innerDistX, y: innerDistY, radius: innerRadius, midAngle },
      outerDist: { x: outerDistX, y: outerDistY, radius: outerRadius, midAngle },
    };
  }

  #getSectorHandlePositions(sector) {
    if (!sector) return null;

    const handleRadius = this.beam_length * 0.5;
    if (handleRadius <= 0) return null;

    const startAngleX = this.center_x + handleRadius * Math.sin(sector.startAngle);
    const startAngleY = this.center_y - handleRadius * Math.cos(sector.startAngle);
    const endAngleX = this.center_x + handleRadius * Math.sin(sector.endAngle);
    const endAngleY = this.center_y - handleRadius * Math.cos(sector.endAngle);

    return {
      startAngle: { x: startAngleX, y: startAngleY, angle: sector.startAngle },
      endAngle: { x: endAngleX, y: endAngleY, angle: sector.endAngle },
    };
  }

  #hitTestHandles(x, y) {
    const hitRadius = 15;

    if (this.editingZoneIndex !== null) {
      const zone = this.guardZones[this.editingZoneIndex];
      if (zone) {
        const handles = this.#getHandlePositions(zone);
        if (handles) {
          for (const [name, pos] of Object.entries(handles)) {
            const dx = x - pos.x;
            const dy = y - pos.y;
            if (dx * dx + dy * dy <= hitRadius * hitRadius) {
              return { type: "zone", handle: name };
            }
          }
        }
      }
    }

    if (this.editingSectorIndex !== null) {
      const sector = this.noTransmitSectors[this.editingSectorIndex];
      if (sector) {
        const handles = this.#getSectorHandlePositions(sector);
        if (handles) {
          for (const [name, pos] of Object.entries(handles)) {
            const dx = x - pos.x;
            const dy = y - pos.y;
            if (dx * dx + dy * dy <= hitRadius * hitRadius) {
              return { type: "sector", handle: name };
            }
          }
        }
      }
    }

    return null;
  }

  #handleMouseDown(event) {
    const coords = this.#getCanvasCoords(event);
    const hit = this.#hitTestHandles(coords.x, coords.y);

    if (hit) {
      if (hit.type === "zone" && this.editingZoneIndex !== null) {
        const zone = this.guardZones[this.editingZoneIndex];
        this.dragState = {
          type: "zone",
          handle: hit.handle,
          startX: coords.x,
          startY: coords.y,
          originalZone: { ...zone },
        };
      } else if (hit.type === "sector" && this.editingSectorIndex !== null) {
        const sector = this.noTransmitSectors[this.editingSectorIndex];
        this.dragState = {
          type: "sector",
          handle: hit.handle,
          startX: coords.x,
          startY: coords.y,
          originalSector: { ...sector },
        };
      }
      this.overlay_dom.style.cursor = "grabbing";
      event.preventDefault();
    }
  }

  #handleMouseMove(event) {
    const coords = this.#getCanvasCoords(event);

    if (this.dragState) {
      if (this.dragState.type === "zone") {
        this.#updateZoneFromDrag(coords.x, coords.y);
      } else if (this.dragState.type === "sector") {
        this.#updateSectorFromDrag(coords.x, coords.y);
      }
      event.preventDefault();
    } else {
      const hit = this.#hitTestHandles(coords.x, coords.y);
      const newHovered = hit ? hit.handle : null;
      if (newHovered !== this.hoveredHandle) {
        this.hoveredHandle = newHovered;
        this.overlay_dom.style.cursor = hit ? "grab" : "default";
        this.redrawCanvas();
      }
    }
  }

  #handleMouseUp(event) {
    if (this.dragState) {
      if (this.dragState.type === "zone") {
        const zoneIndex = this.editingZoneIndex;
        const newZone = this.guardZones[zoneIndex];
        if (this.onZoneDragEnd && newZone) {
          this.onZoneDragEnd(zoneIndex, newZone);
        }
      } else if (this.dragState.type === "sector") {
        const sectorIndex = this.editingSectorIndex;
        const newSector = this.noTransmitSectors[sectorIndex];
        if (this.onSectorDragEnd && newSector) {
          this.onSectorDragEnd(sectorIndex, newSector);
        }
      }
      this.dragState = null;
      this.overlay_dom.style.cursor = this.hoveredHandle ? "grab" : "default";
    }
  }

  #handleTouchStart(event) {
    if (event.touches.length === 1) {
      const touch = event.touches[0];
      const rect = this.overlay_dom.getBoundingClientRect();
      const x = touch.clientX - rect.left;
      const y = touch.clientY - rect.top;
      const hit = this.#hitTestHandles(x, y);

      if (hit) {
        if (hit.type === "zone" && this.editingZoneIndex !== null) {
          const zone = this.guardZones[this.editingZoneIndex];
          this.dragState = {
            type: "zone",
            handle: hit.handle,
            startX: x,
            startY: y,
            originalZone: { ...zone },
          };
          event.preventDefault();
        } else if (hit.type === "sector" && this.editingSectorIndex !== null) {
          const sector = this.noTransmitSectors[this.editingSectorIndex];
          this.dragState = {
            type: "sector",
            handle: hit.handle,
            startX: x,
            startY: y,
            originalSector: { ...sector },
          };
          event.preventDefault();
        }
      }
    }
  }

  #handleTouchMove(event) {
    if (this.dragState && event.touches.length === 1) {
      const touch = event.touches[0];
      const rect = this.overlay_dom.getBoundingClientRect();
      const x = touch.clientX - rect.left;
      const y = touch.clientY - rect.top;
      if (this.dragState.type === "zone") {
        this.#updateZoneFromDrag(x, y);
      } else if (this.dragState.type === "sector") {
        this.#updateSectorFromDrag(x, y);
      }
      event.preventDefault();
    }
  }

  #handleTouchEnd(event) {
    if (this.dragState) {
      if (this.dragState.type === "zone") {
        const zoneIndex = this.editingZoneIndex;
        const newZone = this.guardZones[zoneIndex];
        if (this.onZoneDragEnd && newZone) {
          this.onZoneDragEnd(zoneIndex, newZone);
        }
      } else if (this.dragState.type === "sector") {
        const sectorIndex = this.editingSectorIndex;
        const newSector = this.noTransmitSectors[sectorIndex];
        if (this.onSectorDragEnd && newSector) {
          this.onSectorDragEnd(sectorIndex, newSector);
        }
      }
      this.dragState = null;
    }
  }

  #updateZoneFromDrag(x, y) {
    if (!this.dragState || this.editingZoneIndex === null) return;

    const zone = this.guardZones[this.editingZoneIndex];
    if (!zone) return;

    const radarCoords = this.#pixelToRadarCoords(x, y);

    switch (this.dragState.handle) {
      case "startAngle":
        zone.startAngle = radarCoords.angle;
        break;
      case "endAngle":
        zone.endAngle = radarCoords.angle;
        break;
      case "innerDist":
        zone.startDistance = Math.max(0, radarCoords.distance);
        if (zone.startDistance > zone.endDistance - 50) {
          zone.startDistance = zone.endDistance - 50;
        }
        break;
      case "outerDist":
        zone.endDistance = Math.max(50, radarCoords.distance);
        if (zone.endDistance < zone.startDistance + 50) {
          zone.endDistance = zone.startDistance + 50;
        }
        break;
    }

    this.redrawCanvas();
  }

  #updateSectorFromDrag(x, y) {
    if (!this.dragState || this.editingSectorIndex === null) return;

    const sector = this.noTransmitSectors[this.editingSectorIndex];
    if (!sector) return;

    const radarCoords = this.#pixelToRadarCoords(x, y);

    switch (this.dragState.handle) {
      case "startAngle":
        sector.startAngle = radarCoords.angle;
        break;
      case "endAngle":
        sector.endAngle = radarCoords.angle;
        break;
    }

    this.redrawCanvas();
  }

  #drawDragHandles(ctx, zone) {
    if (!zone) return;

    const handles = this.#getHandlePositions(zone);
    if (!handles) return;

    const handleRadius = 12;

    for (const [name, pos] of Object.entries(handles)) {
      const isHovered = this.hoveredHandle === name;
      const isDragging = this.dragState?.handle === name;

      ctx.save();

      ctx.beginPath();
      ctx.arc(pos.x, pos.y, handleRadius, 0, 2 * Math.PI);

      if (isDragging) {
        ctx.fillStyle = "rgba(255, 255, 255, 0.9)";
      } else if (isHovered) {
        ctx.fillStyle = "rgba(255, 255, 255, 0.7)";
      } else {
        ctx.fillStyle = "rgba(255, 255, 255, 0.5)";
      }
      ctx.fill();

      ctx.strokeStyle = "rgba(100, 100, 100, 0.8)";
      ctx.lineWidth = 2;
      ctx.stroke();

      ctx.translate(pos.x, pos.y);

      if (name === "startAngle" || name === "endAngle") {
        ctx.rotate(pos.angle + Math.PI / 2);
      } else {
        ctx.rotate(handles.innerDist.midAngle);
      }

      ctx.strokeStyle = "rgba(50, 50, 50, 0.9)";
      ctx.lineWidth = 2;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";

      const arrowSize = 5;
      const arrowLength = 6;

      ctx.beginPath();
      ctx.moveTo(-arrowLength, 0);
      ctx.lineTo(arrowLength, 0);
      ctx.moveTo(arrowLength - arrowSize, -arrowSize);
      ctx.lineTo(arrowLength, 0);
      ctx.lineTo(arrowLength - arrowSize, arrowSize);
      ctx.moveTo(-arrowLength + arrowSize, -arrowSize);
      ctx.lineTo(-arrowLength, 0);
      ctx.lineTo(-arrowLength + arrowSize, arrowSize);
      ctx.stroke();

      ctx.restore();
    }
  }

  #drawSectorDragHandles(ctx, sector) {
    if (!sector) return;

    const handles = this.#getSectorHandlePositions(sector);
    if (!handles) return;

    const handleRadius = 12;

    for (const [name, pos] of Object.entries(handles)) {
      const isHovered = this.hoveredHandle === name;
      const isDragging = this.dragState?.handle === name;

      ctx.save();

      ctx.beginPath();
      ctx.arc(pos.x, pos.y, handleRadius, 0, 2 * Math.PI);

      if (isDragging) {
        ctx.fillStyle = "rgba(255, 255, 255, 0.9)";
      } else if (isHovered) {
        ctx.fillStyle = "rgba(255, 255, 255, 0.7)";
      } else {
        ctx.fillStyle = "rgba(255, 255, 255, 0.5)";
      }
      ctx.fill();

      ctx.strokeStyle = "rgba(100, 100, 100, 0.8)";
      ctx.lineWidth = 2;
      ctx.stroke();

      ctx.translate(pos.x, pos.y);
      ctx.rotate(pos.angle + Math.PI / 2);

      ctx.strokeStyle = "rgba(50, 50, 50, 0.9)";
      ctx.lineWidth = 2;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";

      const arrowSize = 5;
      const arrowLength = 6;

      ctx.beginPath();
      ctx.moveTo(-arrowLength, 0);
      ctx.lineTo(arrowLength, 0);
      ctx.moveTo(arrowLength - arrowSize, -arrowSize);
      ctx.lineTo(arrowLength, 0);
      ctx.lineTo(arrowLength - arrowSize, arrowSize);
      ctx.moveTo(-arrowLength + arrowSize, -arrowSize);
      ctx.lineTo(-arrowLength, 0);
      ctx.lineTo(-arrowLength + arrowSize, arrowSize);
      ctx.stroke();

      ctx.restore();
    }
  }
}
