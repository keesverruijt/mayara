export { render_2d };

import { RANGE_SCALE } from "./viewer.js";

class render_2d {
  // The constructor gets two canvases, the real drawing one and one for background data
  // such as range circles etc.
  constructor(canvas_dom, canvas_background_dom, drawBackground) {
    this.dom = canvas_dom;
    this.background_dom = canvas_background_dom;
    this.drawBackgroundCallback = drawBackground;
    this.redrawCanvas();
  }

  // This is called as soon as it is clear what the number of spokes and their max length is
  // Some brand vary the spoke length with data or range, but a promise is made about the
  // max length.
  setSpokes(spokesPerRevolution, max_spoke_len) {
    this.spokesPerRevolution = spokesPerRevolution;
    this.max_spoke_len = max_spoke_len;
  }

  setRange(range) {
    this.range = range;
    this.redrawCanvas();
  }

  // A new "legend" of what each byte means in terms of suggested color and meaning.
  // The index is the byte value in the spoke.
  // Each entry contains a four byte array of colors and alpha (x,y,z,a).
  setLegend(l) {
    this.legend = l;
  }

  // Called on initial setup and whenever the canvas size changes.
  redrawCanvas() {
    var parent = this.dom.parentNode,
      styles = getComputedStyle(parent),
      w = parseInt(styles.getPropertyValue("width"), 10),
      h = parseInt(styles.getPropertyValue("height"), 10);

    this.dom.width = w;
    this.dom.height = h;
    this.background_dom.width = w;
    this.background_dom.height = h;

    this.width = this.dom.width;
    this.height = this.dom.height;
    this.center_x = this.width / 2;
    this.center_y = this.height / 2;
    this.beam_length = Math.trunc(
      Math.max(this.center_x, this.center_y) * RANGE_SCALE
    );
    this.ctx = this.dom.getContext("2d", { alpha: true });
    this.background_ctx = this.background_dom.getContext("2d");

    this.pattern = document.createElement("canvas");
    this.pattern.width = 2048;
    this.pattern.height = 1;
    this.pattern_ctx = this.pattern.getContext("2d");
    this.image = this.pattern_ctx.createImageData(2048, 1);

    this.drawBackgroundCallback(this, "MAYARA (Canvas 2D)");
  }

  // A new spoke has been received.
  // The spoke object contains:
  // - angle: the angle [0, spokesPerRevolution> relative to the front of the boat, clockwise.
  // - bearing: optional angle [0, spokesPerRevolution> relative to true north.
  // - range: actual range for furthest pixel, this can be (very) different from the
  //          official range passed via range().
  // - data: spoke data from closest to furthest from radome. Each byte value can be
  //         looked up in the legend.
  drawSpoke(spoke) {
    let a =
      (2 *
        Math.PI *
        ((spoke.angle + (this.spokesPerRevolution * 3) / 4) %
          this.spokesPerRevolution)) /
      this.spokesPerRevolution;
    let pixels_per_item = (this.beam_length * RANGE_SCALE) / spoke.data.length;
    if (this.range) {
      pixels_per_item = (pixels_per_item * spoke.range) / this.range;
    }
    let c = Math.cos(a) * pixels_per_item;
    let s = Math.sin(a) * pixels_per_item;

    for (let i = 0, idx = 0; i < spoke.data.length; i++, idx += 4) {
      let v = spoke.data[i];

      this.image.data[idx + 0] = this.legend[v][0];
      this.image.data[idx + 1] = this.legend[v][1];
      this.image.data[idx + 2] = this.legend[v][2];
      this.image.data[idx + 3] = this.legend[v][3];
    }

    this.pattern_ctx.putImageData(this.image, 0, 0);

    let pattern = this.ctx.createPattern(this.pattern, "repeat-x");

    let arc_angle = (2 * Math.PI) / this.spokesPerRevolution;

    this.ctx.setTransform(c, s, -s, c, this.center_x, this.center_y);
    this.ctx.fillStyle = pattern;
    this.ctx.beginPath();
    this.ctx.moveTo(0, 0);
    this.ctx.arc(0, 0, spoke.data.length, 0, arc_angle);
    this.ctx.closePath();
    this.ctx.fill();
  }

  // A number of spokes has been received and now is a good time to render
  // them to the screen. Usually every 14-32 spokes.
  render() {}
}
