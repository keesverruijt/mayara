export { SpokeProcessorFactory };

/**
 * Base class for spoke processing strategies
 */
class SpokeProcessor {
  constructor(legend) {
    this.legend = legend;
    this.rotationCount = 0;
    this.lastSpokeAngle = -1;
    this.firstSpokeAngle = -1;
  }

  /**
   * Reset state when clearing display or changing range
   */
  reset() {
    this.rotationCount = 0;
    this.lastSpokeAngle = -1;
    this.firstSpokeAngle = -1;
  }

  /**
   * Update rotation tracking
   * @param {number} spokeAngle - Current spoke angle
   * @param {number} spokesPerRevolution - Total spokes per revolution
   * @returns {boolean} - True if rotation wrapped around
   */
  updateRotationTracking(spokeAngle, spokesPerRevolution) {
    if (this.firstSpokeAngle < 0) {
      this.firstSpokeAngle = spokeAngle;
    }

    // Detect when we wrap around from high angle to low angle
    const wrapped =
      this.lastSpokeAngle >= 0 &&
      spokeAngle < this.lastSpokeAngle - spokesPerRevolution / 2;

    if (wrapped) {
      this.rotationCount++;
    }

    this.lastSpokeAngle = spokeAngle;
    return wrapped;
  }

  /**
   * Check if processor needs to wait for a full rotation before displaying
   * after standby/range change (to flush stale buffered spokes)
   * @returns {boolean}
   */
  needsRotationWait() {
    return false; // Override in subclasses if needed
  }

  /**
   * Process spoke data into the buffer
   * @param {Uint8Array} buffer - Output buffer
   * @param {Object} spoke - Spoke data with angle and data
   * @param {number} spokesPerRevolution - Total spokes per revolution
   * @param {number} maxSpokeLength - Maximum spoke length
   */
  processSpoke(buffer, spoke, spokesPerRevolution, maxSpokeLength) {
    throw new Error("processSpoke must be implemented by subclass");
  }
}

/**
 * Clean processor - no smoothing, displays spokes as-is immediately
 */
class CleanSpokeProcessor extends SpokeProcessor {
  processSpoke(buffer, spoke, spokesPerRevolution, maxSpokeLength) {
    const offset = spoke.angle * maxSpokeLength;
    const spokeLen = spoke.data.length;

    // Bounds check
    if (offset + spokeLen > buffer.length) {
      console.error(
        `Buffer overflow: offset=${offset}, data.len=${spokeLen}, buf.len=${buffer.length}`
      );
      return;
    }

    // Write spoke data directly without modification
    for (let i = 0; i < spokeLen; i++) {
      buffer[offset + i] = spoke.data[i];
    }

    // Clear remainder of spoke if data is shorter than max
    if (spokeLen < maxSpokeLength) {
      buffer.fill(0, offset + spokeLen, offset + maxSpokeLength);
    }
  }
}

/**
 * Smoothing processor - uses neighbor enhancement and filtering
 * Waits for several rotations before applying aggressive filtering
 */
class SmoothingSpokeProcessor extends SpokeProcessor {
  constructor(legend) {
    super(legend);
    this.fillRotations = 4; // Number of rotations to use neighbor enhancement
  }

  needsRotationWait() {
    // Smoothing needs to wait for a full rotation to flush stale buffered spokes
    return true;
  }

  processSpoke(buffer, spoke, spokesPerRevolution, maxSpokeLength) {
    const offset = spoke.angle * maxSpokeLength;
    const spokeLen = spoke.data.length;

    // Bounds check
    if (offset + spokeLen > buffer.length) {
      console.error(
        `Buffer overflow: offset=${offset}, data.len=${spokeLen}, buf.len=${buffer.length}`
      );
      return;
    }

    const strongReturn = this.legend.strongReturn;
    const mediumReturn = this.legend.mediumReturn;
    const lowReturn = this.legend.lowReturn;
    const specialStart = this.legend.specialStart;
    const maxNormal = specialStart - 1;

    // Only use neighbor enhancement during first few rotations to fill display quickly
    if (this.rotationCount < this.fillRotations) {
      this.#neighborEnhancement(
        buffer,
        spoke,
        spokesPerRevolution,
        maxSpokeLength,
        offset,
        spokeLen,
        strongReturn,
        mediumReturn,
        specialStart
      );
    } else {
      this.#smartFiltering(
        buffer,
        spoke,
        spokesPerRevolution,
        maxSpokeLength,
        offset,
        spokeLen,
        strongReturn,
        mediumReturn,
        lowReturn,
        specialStart,
        maxNormal
      );
    }

    // Clear remainder of spoke if data is shorter than max
    if (spokeLen < maxSpokeLength) {
      buffer.fill(0, offset + spokeLen, offset + maxSpokeLength);
    }
  }

  #neighborEnhancement(
    buffer,
    spoke,
    spokesPerRevolution,
    maxSpokeLength,
    offset,
    spokeLen,
    strongReturn,
    mediumReturn,
    specialStart
  ) {
    for (let i = 0; i < spokeLen; i++) {
      const val = spoke.data[i];
      // Write current spoke at full value
      buffer[offset + i] = val;

      if (val >= specialStart) {
        // Leave things like history, doppler etc as they are
        continue;
      }

      if (val > 1) {
        // Strong signals (>60): spread wide (±6 spokes) with higher intensity
        // Medium signals (25-60): spread medium (±4 spokes)
        // Weak signals (<25): spread narrow (±2 spokes) with lower intensity
        let spreadWidth, blendFactors;

        if (val > strongReturn) {
          // Strong signal - spread wide and strong
          spreadWidth = 6;
          blendFactors = [0.95, 0.88, 0.78, 0.65, 0.5, 0.35];
        } else if (val > mediumReturn) {
          // Medium signal - normal spread
          spreadWidth = 4;
          blendFactors = [0.85, 0.65, 0.45, 0.25];
        } else {
          // Weak signal - narrow spread, lower intensity
          spreadWidth = 2;
          blendFactors = [0.6, 0.3];
        }

        // Spread to neighboring spokes (both directions)
        for (let d = 1; d <= spreadWidth; d++) {
          const prev = (spoke.angle + spokesPerRevolution - d) % spokesPerRevolution;
          const next = (spoke.angle + d) % spokesPerRevolution;
          const prevOffset = prev * maxSpokeLength;
          const nextOffset = next * maxSpokeLength;
          const blendVal = Math.floor(val * blendFactors[d - 1]);

          if (buffer[prevOffset + i] < blendVal) {
            buffer[prevOffset + i] = blendVal;
          }
          if (buffer[nextOffset + i] < blendVal) {
            buffer[nextOffset + i] = blendVal;
          }
        }
      }
    }
  }

  #smartFiltering(
    buffer,
    spoke,
    spokesPerRevolution,
    maxSpokeLength,
    offset,
    spokeLen,
    strongReturn,
    mediumReturn,
    lowReturn,
    specialStart,
    maxNormal
  ) {
    // Smart filtering
    // - Strong signals with neighbor support get amplified aggressively (wide check ±4)
    // - Isolated weak signals (scatter) get killed

    // Wide neighbor check for strong signals: ±4 spokes
    const prev1Offset = ((spoke.angle + spokesPerRevolution - 1) % spokesPerRevolution) * maxSpokeLength;
    const prev2Offset = ((spoke.angle + spokesPerRevolution - 2) % spokesPerRevolution) * maxSpokeLength;
    const prev3Offset = ((spoke.angle + spokesPerRevolution - 3) % spokesPerRevolution) * maxSpokeLength;
    const prev4Offset = ((spoke.angle + spokesPerRevolution - 4) % spokesPerRevolution) * maxSpokeLength;
    const next1Offset = ((spoke.angle + 1) % spokesPerRevolution) * maxSpokeLength;
    const next2Offset = ((spoke.angle + 2) % spokesPerRevolution) * maxSpokeLength;
    const next3Offset = ((spoke.angle + 3) % spokesPerRevolution) * maxSpokeLength;
    const next4Offset = ((spoke.angle + 4) % spokesPerRevolution) * maxSpokeLength;

    for (let i = 0; i < spokeLen; i++) {
      const val = spoke.data[i];

      // Check neighbor support (from previous rotation's data still in buffer)
      const prev1 = buffer[prev1Offset + i];
      const prev2 = buffer[prev2Offset + i];
      const prev3 = buffer[prev3Offset + i];
      const prev4 = buffer[prev4Offset + i];
      const next1 = buffer[next1Offset + i];
      const next2 = buffer[next2Offset + i];
      const next3 = buffer[next3Offset + i];
      const next4 = buffer[next4Offset + i];

      // For strong signals: use wide sum (±4)
      const wideSum = prev1 + prev2 + prev3 + prev4 + next1 + next2 + next3 + next4;
      const wideMax = Math.max(prev1, prev2, prev3, prev4, next1, next2, next3, next4);
      // For weak signals: use narrow sum (±2)
      const narrowSum = prev1 + prev2 + next1 + next2;
      const narrowMax = Math.max(prev1, prev2, next1, next2);

      let outputVal;

      if (val > strongReturn) {
        // Strong signal: use wide neighbor check (±4)
        if (wideSum > 3 * strongReturn) {
          // Solid mass - boost hard and spread to neighbors
          outputVal = Math.min(maxNormal, Math.floor(val * 1.35));
          // Boost immediate neighbors to fill gaps
          if (prev1 > mediumReturn)
            buffer[prev1Offset + i] = Math.min(maxNormal, Math.floor(prev1 * 1.15));
          if (next1 > mediumReturn)
            buffer[next1Offset + i] = Math.min(maxNormal, Math.floor(next1 * 1.15));
          if (prev2 > mediumReturn)
            buffer[prev2Offset + i] = Math.min(maxNormal, Math.floor(prev2 * 1.1));
          if (next2 > mediumReturn)
            buffer[next2Offset + i] = Math.min(maxNormal, Math.floor(next2 * 1.1));
        } else if (wideMax > mediumReturn) {
          // Some support - moderate boost
          outputVal = Math.min(maxNormal, Math.floor(val * 1.2));
        } else {
          // Strong but isolated - suspicious, reduce
          outputVal = Math.floor(val * 0.8);
        }
      } else if (val > mediumReturn) {
        // Medium signal: needs good neighbor support
        if (narrowSum > 3 * mediumReturn) {
          // Good support - boost it
          outputVal = Math.min(maxNormal, Math.floor(val * 1.2));
        } else if (narrowMax > 2 * mediumReturn) {
          // Some support - keep
          outputVal = val;
        } else {
          // Isolated medium - likely scatter, punish hard
          outputVal = Math.floor(val * 0.4);
        }
      } else if (val > 1) {
        // Weak signal: kill it unless very well supported
        if (narrowSum > 3 * mediumReturn) {
          // Strong neighbors - this might be edge of real target
          outputVal = val;
        } else if (narrowMax > strongReturn) {
          // Next to something strong - keep faint
          outputVal = Math.floor(val * 0.5);
        } else {
          // Isolated weak signal - kill it
          outputVal = 0;
        }
      } else {
        outputVal = val;
      }

      buffer[offset + i] = val; // outputVal; (currently disabled in original)
    }
  }
}

/**
 * Factory for creating spoke processors
 */
class SpokeProcessorFactory {
  /**
   * Create a spoke processor
   * @param {string} mode - "auto", "clean", or "smoothing"
   * @param {number} spokesPerRevolution - Number of spokes per revolution
   * @param {Object} legend - Legend with strongReturn, mediumReturn, etc.
   * @returns {SpokeProcessor}
   */
  static create(mode, spokesPerRevolution, legend) {
    if (mode === "auto") {
      // Auto-detect: <= 2048 spokes use clean, > 2048 use smoothing
      mode = spokesPerRevolution <= 2048 ? "clean" : "smoothing";
    }

    switch (mode) {
      case "clean":
        return new CleanSpokeProcessor(legend);
      case "smoothing":
        return new SmoothingSpokeProcessor(legend);
      default:
        throw new Error(`Unknown spoke processor mode: ${mode}`);
    }
  }
}
