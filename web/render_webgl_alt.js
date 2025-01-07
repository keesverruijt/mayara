export { render_webgl_alt };

import { RANGE_SCALE } from "./viewer.js";

class render_webgl_alt {
  // The constructor gets two canvases, the real drawing one and one for background data
  // such as range circles etc.
  constructor(canvas_dom, canvas_background_dom) {
    this.dom = canvas_dom;
    this.background_dom = canvas_background_dom;
    this.background_ctx = this.background_dom.getContext("2d");

    let gl = this.dom.getContext("webgl2", {
      preserveDrawingBuffer: true,
    });
    if (gl == null) {
      throw new Error("Error creating Webgl2 context for radar");
    }
    let vertexBuffer = gl.createBuffer();
    if (vertexBuffer == null) {
      throw new Error("Error creating vertexbuffer");
    }
    let colorBuffer = gl.createBuffer();
    if (colorBuffer == null) {
      throw new Error("Error creating colorBuffer");
    }

    let vertCode =
      "attribute vec3 coordinates;" +
      "attribute vec4 color;" +
      "uniform mat4 u_transform;" +
      "varying vec4 vColor;" +
      "void main(void) {" +
      "  gl_Position = u_transform * vec4(coordinates, 1.0);" +
      "  vColor = color;" +
      "}";
    // Create a vertex shader object
    let vertShader = gl.createShader(gl.VERTEX_SHADER);
    // Attach vertex shader source code
    gl.shaderSource(vertShader, vertCode);
    // Compile the vertex shader
    gl.compileShader(vertShader);
    if (!gl.getShaderParameter(vertShader, gl.COMPILE_STATUS)) {
      throw new Error(gl.getShaderInfoLog(vertShader));
    }
    // fragment shader source code
    let fragCode =
      "precision mediump float;" +
      "varying vec4 vColor;" +
      "void main(void) {" +
      "gl_FragColor = vColor;" +
      "}";
    // Create fragment shader object
    let fragShader = gl.createShader(gl.FRAGMENT_SHADER);
    // Attach fragment shader source code
    gl.shaderSource(fragShader, fragCode);
    // Compile the fragmentt shader
    gl.compileShader(fragShader);
    if (!gl.getShaderParameter(fragShader, gl.COMPILE_STATUS)) {
      throw new Error(gl.getShaderInfoLog(fragShader));
    }
    // Create a shader program object to store
    // the combined shader program
    let shaderProgram = gl.createProgram();
    // Attach a vertex shader
    gl.attachShader(shaderProgram, vertShader);
    // Attach a fragment shader
    gl.attachShader(shaderProgram, fragShader);
    // Link both programs
    gl.linkProgram(shaderProgram);
    if (!gl.getProgramParameter(shaderProgram, gl.LINK_STATUS)) {
      throw new Error(gl.getProgramInfoLog(shaderProgram));
    }
    // Use the combined shader program object
    gl.useProgram(shaderProgram);
    /*======== Associating shaders to buffer objects ========*/
    this.transform_matrix_location = gl.getUniformLocation(
      shaderProgram,
      "u_transform"
    );

    // Bind vertex buffer object
    gl.bindBuffer(gl.ARRAY_BUFFER, vertexBuffer);
    // Get the attribute location
    var coordAttr = gl.getAttribLocation(shaderProgram, "coordinates");
    // Point an attribute to the currently bound VBO
    gl.vertexAttribPointer(coordAttr, 3, gl.FLOAT, false, 0, 0);
    // Enable the attribute
    gl.enableVertexAttribArray(coordAttr);
    // bind the color buffer
    gl.bindBuffer(gl.ARRAY_BUFFER, colorBuffer);
    // get the attribute location
    var colorAttr = gl.getAttribLocation(shaderProgram, "color");
    // point attribute to the color buffer object
    gl.vertexAttribPointer(colorAttr, 4, gl.FLOAT, false, 0, 0);
    // enable the color attribute
    gl.enableVertexAttribArray(colorAttr);
    gl.clearColor(0.0, 0.0, 0.0, 0.0);
    gl.clear(gl.COLOR_BUFFER_BIT);

    this.vertexBuffer = vertexBuffer;
    this.colorBuffer = colorBuffer;
    this.gl = gl;
    this.actual_range = 0;
    this.heading = 0;

    this.redrawCanvas();
  }

  #angleToBearing(angle) {
    let h = this.heading - 90;
    if (h < 0) {
      h += 360;
    }
    angle += Math.round(h / (360 / this.spokes)); // add heading
    angle = angle % this.spokes;
    return angle;
  }

  // This is called as soon as it is clear what the number of spokes and their max length is
  // Some brand vary the spoke length with data or range, but a promise is made about the
  // max length.
  setSpokes(spokes, max_spoke_len) {
    this.spokes = spokes;
    this.max_spoke_len = max_spoke_len;

    //build positions
    let x = [];
    let y = [];
    const cx = 0;
    const cy = 0;
    const maxRadius = 1;
    const angleShift = (2 * Math.PI) / this.spokes / 2;
    const radiusShift = 0.0; // (1 / this.max_spoke_len)/2
    for (let a = 0; a < this.spokes; a++) {
      for (let r = 0; r < this.max_spoke_len; r++) {
        const angle = a * ((2 * Math.PI) / this.spokes) + angleShift;
        const radius = r * (maxRadius / this.max_spoke_len);
        const x1 = cx + (radius + radiusShift) * Math.cos(angle);
        const y1 = cy + (radius + radiusShift) * Math.sin(angle);
        x[a * this.max_spoke_len + r] = x1;
        y[a * this.max_spoke_len + r] = -y1;
      }
    }
    this.x = x;
    this.y = y;
  }

  setRange(range) {
    this.range = range;
    this.redrawCanvas();
  }

  // A new "legend" of what each byte means in terms of suggested color and meaning.
  // The index is the byte value in the spoke.
  // Each entry contains a four byte array of colors and alpha (x,y,z,a).
  setLegend(l) {
    // Scale the data to OpenGL scaling values (u8 -> float(1.0))
    let a = Array();
    for (let i = 0; i < Object.keys(l).length; i++) {
      let color = l[i];
      color[0] = color[0] / 255;
      color[1] = color[1] / 255;
      color[2] = color[2] / 255;
      color[3] = color[3] / 255;
      a.push(color);
    }
    this.legend = a;
  }

  // A new spoke has been received.
  // The spoke object contains:
  // - angle: the angle [0, max_spokes> relative to the front of the boat, clockwise.
  // - bearing: optional angle [0, max_spokes> relative to true north.
  // - range: actual range for furthest pixel, this can be (very) different from the
  //          official range passed via range().
  // - data: spoke data from closest to furthest from radome. Each byte value can be
  //         looked up in the legend.
  drawSpoke(spoke) {
    if (!this.x) {
      return;
    }
    let gl = this.gl;

    if (this.actual_range != spoke.range) {
      this.actual_range = spoke.range;
      this.redrawCanvas();
    }
    let spokeBearing = spoke.has_bearing
      ? spoke.bearing
      : this.#angleToBearing(spoke.angle);
    let ba = spokeBearing + 1;
    if (ba > this.spokes - 1) {
      ba = 0;
    }
    // draw current spoke
    let offset = spokeBearing * this.max_spoke_len;
    let next_offset = ba * this.max_spoke_len;
    for (let i = 0; i < spoke.data.length; i++) {
      this.vertices.push(this.x[offset + i]);
      this.vertices.push(this.y[offset + i]);
      this.vertices.push(0.0);
      this.vertices.push(this.x[next_offset + i]);
      this.vertices.push(this.y[next_offset + i]);
      this.vertices.push(0.0);
      let color = this.legend[spoke.data[i]];
      if (color) {
        this.verticeColors.push(color[0]);
        this.verticeColors.push(color[1]);
        this.verticeColors.push(color[2]);
        this.verticeColors.push(color[3]);
        this.verticeColors.push(color[0]);
        this.verticeColors.push(color[1]);
        this.verticeColors.push(color[2]);
        this.verticeColors.push(color[3]);
      } else {
        this.verticeColors.push(1.0);
        this.verticeColors.push(1.0);
        this.verticeColors.push(1.0);
        this.verticeColors.push(0);
        this.verticeColors.push(1.0);
        this.verticeColors.push(1.0);
        this.verticeColors.push(1.0);
        this.verticeColors.push(0);
      }
    }
  }

  // A number of spokes has been received and now is a good time to render
  // them to the screen. Usually every 14-32 spokes.
  render() {
    if (!this.x) {
      return;
    }

    let gl = this.gl;

    // Draw buffer
    gl.bindBuffer(gl.ARRAY_BUFFER, this.vertexBuffer);
    gl.bufferData(
      gl.ARRAY_BUFFER,
      new Float32Array(this.vertices),
      gl.STATIC_DRAW
    );
    gl.bindBuffer(gl.ARRAY_BUFFER, this.colorBuffer);
    gl.bufferData(
      gl.ARRAY_BUFFER,
      new Float32Array(this.verticeColors),
      gl.STATIC_DRAW
    );
    gl.bindBuffer(gl.ARRAY_BUFFER, null);
    gl.drawArrays(gl.LINES, 0, this.vertices.length / 3);

    this.vertices = [];
    this.verticeColors = [];
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

    this.drawBackgroundCallback(this, "MAYARA (WebGL Alt)");

    this.#setTransformationMatrix();

    this.gl.viewport(0, 0, w, h);

    this.vertices = [];
    this.verticeColors = [];
  }

  #setTransformationMatrix() {
    let scale = (RANGE_SCALE * this.actual_range) / this.range;

    this.transform_matrix = new Float32Array([
      scale * ((2 * this.beam_length) / this.width),
      0.0,
      0.0,
      0.0,
      0.0,
      scale * ((2 * this.beam_length) / this.height),
      0.0,
      0.0,
      0.0,
      0.0,
      1.0,
      0.0,
      0.0,
      0.0,
      0.0,
      1.0,
    ]);
    this.gl.uniformMatrix4fv(
      this.transform_matrix_location,
      false,
      this.transform_matrix
    );

    this.gl.clear(this.gl.COLOR_BUFFER_BIT);

    this.background_ctx.fillStyle = "lightgreen";
    this.background_ctx.fillText("Beamlength " + this.beam_length, 5, 40);
    this.background_ctx.fillText("Range " + this.range, 5, 60);
    this.background_ctx.fillText("Spoke " + this.actual_range, 5, 80);
  }
}
