export { render_webgl };

import { RANGE_SCALE } from "./viewer.js";

class render_webgl {
  // The constructor gets two canvases, the real drawing one and one for background data
  // such as range circles etc.
  constructor(canvas_dom, canvas_background_dom) {
    this.dom = canvas_dom;
    this.background_dom = canvas_background_dom;

    this.gl = init(this.dom);

    this.actual_range = 0;
  }

  // This is called as soon as it is clear what the number of spokes and their max length is
  // Some brand vary the spoke length with data or range, but a promise is made about the
  // max length.
  setSpokes(spokes, max_spoke_len) {
    this.spokes = spokes;
    this.max_spoke_len = max_spoke_len;
    this.data = loadTexture(this.gl, spokes, max_spoke_len);
  }

  // An updated range, and an optional array of descriptions. The array may be null.
  setRange(range, descriptions) {
    this.range = range;
    this.rangeDescriptions = descriptions;
    this.redrawCanvas();
  }

  // A new "legend" of what each byte means in terms of suggested color and meaning.
  // The index is the byte value in the spoke.
  // Each entry contains a four byte array of colors and alpha (x,y,z,a).
  setLegend(l) {
    // Create a Uint8Array to hold RGBA data for the color table
    const colorTableData = new Uint8Array(256 * 4); // RGBA for each index

    // Fill the array with example color data (you would replace this with your actual RGBA data)
    for (let i = 0; i < l.length; i++) {
      colorTableData[i * 4] = l[i][0]; // Red channel
      colorTableData[i * 4 + 1] = l[i][1];
      colorTableData[i * 4 + 2] = l[i][2]; // Blue channel
      colorTableData[i * 4 + 3] = l[i][3];
    }

    loadColorTableTexture(this.gl, colorTableData);
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
    if (!this.data) {
      return;
    }
    if (this.actual_range != spoke.range) {
      this.actual_range = spoke.range;
      this.redrawCanvas();
    }
    let offset = spoke.angle * this.max_spoke_len;
    this.data.set(spoke.data, offset);
    if (spoke.data.length < this.max_spoke_len) {
      this.data.fill(
        0,
        offset + spoke.data.length,
        offset + this.max_spoke_len
      );
    }
  }

  // A number of spokes has been received and now is a good time to render
  // them to the screen. Usually every 14-32 spokes.
  render() {
    if (!this.data || !this.spokes) {
      return;
    }
    let gl = this.gl;
    updateTexture(gl, this.data, this.spokes, this.max_spoke_len);
    draw(gl);
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
    this.background_ctx = this.background_dom.getContext("2d");

    this.#drawRings();
  }

  #drawRings() {
    this.background_ctx.setTransform(1, 0, 0, 1, 0, 0);
    this.background_ctx.clearRect(0, 0, this.width, this.height);

    this.background_ctx.strokeStyle = "white";
    this.background_ctx.fillStyle = "white";
    this.background_ctx.font = "bold 16px/1 Verdana, Geneva, sans-serif";
    for (let i = 0; i <= 4; i++) {
      this.background_ctx.beginPath();
      this.background_ctx.arc(
        this.center_x,
        this.center_y,
        (i * this.beam_length) / 4,
        0,
        2 * Math.PI
      );
      this.background_ctx.stroke();
      if (i > 0 && this.range) {
        let r = Math.trunc((this.range * i) / 4);
        console.log("i=" + i + " range=" + this.range + " r=" + r);
        let text = this.rangeDescriptions
          ? this.rangeDescriptions[r]
          : undefined;
        if (text === undefined) {
          if (r % 1000 == 0) {
            text = r / 1000 + " km";
          } else {
            text = r + " m";
          }
        }
        this.background_ctx.fillText(
          text,
          this.center_x + (i * this.beam_length * 1.41) / 8,
          this.center_y + (i * this.beam_length * -1.41) / 8
        );
      }
    }

    this.background_ctx.fillStyle = "lightblue";
    this.background_ctx.fillText("MAYARA (WEBGL CONTEXT)", 5, 20);

    this.gl.clear(this.gl.COLOR_BUFFER_BIT);

    this.background_ctx.fillStyle = "lightgreen";
    this.background_ctx.fillText("Beamlength " + this.beam_length, 5, 40);
    this.background_ctx.fillText("Range " + this.range, 5, 60);
    this.background_ctx.fillText("Spoke " + this.actual_range, 5, 80);
  }
}

const vertexShaderSource = `#version 300 es
  in vec4 a_position;
  in vec2 a_texCoord;
  out vec2 v_texCoord;

  void main() {
    gl_Position = a_position;
    v_texCoord = a_texCoord;
  }
`;

const fragmentShaderSource = `#version 300 es
  precision mediump float;

  in vec2 v_texCoord;
  out vec4 color;

  uniform sampler2D u_polarIndexData; // Polar data texture (contains indices)
  uniform sampler2D u_colorTable;     // Color table texture (contains RGBA values)
  
  void main() {
    // Convert texture coordinates into polar coordinates
    vec2 centeredCoords = v_texCoord - vec2(0.5, 0.5); // Center the coords at (0.5, 0.5)
    float r = length(centeredCoords); // Compute the radius
    float theta = atan(centeredCoords.y, centeredCoords.x); // Compute the angle (theta)
    
    // Normalize theta to be in the range [0, 1] for texture sampling
    float normalizedTheta = 1.0 - (theta + 3.14159265) / (2.0 * 3.14159265); // Map [-π, π] to [0, 1]

     // Sample the index from the polar data texture
    float index = texture(u_polarIndexData, vec2(r, normalizedTheta)).r;

    // Use the index to look up the color in the color table (1D texture)
    vec4 color1 = texture(u_colorTable, vec2(index, 0.0)); 
    // color = vec4(index * 8.0, color1.r, color1.b, 1.0);
    color = color1;
  }
`;

function createShader(gl, type, source) {
  const shader = gl.createShader(type);
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    console.error("Shader compile failed: ", gl.getShaderInfoLog(shader));
    gl.deleteShader(shader);
    return null;
  }
  return shader;
}

function createProgram(gl, vertexShader, fragmentShader) {
  const program = gl.createProgram();
  gl.attachShader(program, vertexShader);
  gl.attachShader(program, fragmentShader);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    console.error("Program link failed: ", gl.getProgramInfoLog(program));
    gl.deleteProgram(program);
    return null;
  }
  return program;
}

function init(canvas) {
  const gl = canvas.getContext("webgl2");

  if (!gl) {
    throw new Error("WebGL2 not supported");
  }

  const vertexShader = createShader(gl, gl.VERTEX_SHADER, vertexShaderSource);
  const fragmentShader = createShader(
    gl,
    gl.FRAGMENT_SHADER,
    fragmentShaderSource
  );
  const program = createProgram(gl, vertexShader, fragmentShader);

  const positionBuffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, positionBuffer);
  const positions = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(positions), gl.STATIC_DRAW);

  const texCoordBuffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, texCoordBuffer);
  const texCoords = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(texCoords), gl.STATIC_DRAW);

  gl.viewport(0, 0, gl.canvas.width, gl.canvas.height);
  gl.clear(gl.COLOR_BUFFER_BIT);

  gl.useProgram(program);

  const positionLocation = gl.getAttribLocation(program, "a_position");
  gl.enableVertexAttribArray(positionLocation);
  gl.bindBuffer(gl.ARRAY_BUFFER, positionBuffer);
  gl.vertexAttribPointer(positionLocation, 2, gl.FLOAT, false, 0, 0);

  const texCoordLocation = gl.getAttribLocation(program, "a_texCoord");
  gl.enableVertexAttribArray(texCoordLocation);
  gl.bindBuffer(gl.ARRAY_BUFFER, texCoordBuffer);
  gl.vertexAttribPointer(texCoordLocation, 2, gl.FLOAT, false, 0, 0);

  gl.activeTexture(gl.TEXTURE0);
  const texture = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, texture);
  const samplerLocation = gl.getUniformLocation(program, "u_polarIndexData");
  gl.uniform1i(samplerLocation, 0);

  gl.activeTexture(gl.TEXTURE1);
  const colorLocation = gl.getUniformLocation(program, "u_colorTable");
  gl.uniform1i(colorLocation, 1);

  return gl;
}

function draw(gl) {
  // Clear the canvas
  gl.clearColor(0.1, 0.3, 0.1, 1.0);
  gl.clear(gl.COLOR_BUFFER_BIT);

  // Set the view port
  gl.viewport(0, 0, gl.canvas.width, gl.canvas.height);

  // Draw the square via two triangles. This is morphed to polar data
  // by the fragment shader.
  gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
}

function loadTexture(gl, spokes, max_spoke_len) {
  let data = new Uint8Array(spokes * max_spoke_len);

  return data;
}

function updateTexture(gl, data, spokes, max_spoke_len) {
  gl.activeTexture(gl.TEXTURE0);
  gl.texImage2D(
    gl.TEXTURE_2D,
    0,
    gl.R8,
    max_spoke_len,
    spokes,
    0,
    gl.RED,
    gl.UNSIGNED_BYTE,
    data
  );
  gl.generateMipmap(gl.TEXTURE_2D);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
}

function loadColorTableTexture(gl, legend) {
  // Create texture for color table
  gl.activeTexture(gl.TEXTURE1);
  const texture = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texImage2D(
    gl.TEXTURE_2D,
    0,
    gl.RGBA8,
    legend.length / 4,
    1,
    0,
    gl.RGBA,
    gl.UNSIGNED_BYTE,
    legend
  );

  // Set texture parameters
  gl.generateMipmap(gl.TEXTURE_2D);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);

  return texture;
}
