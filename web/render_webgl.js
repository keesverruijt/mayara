export { render_webgl };

class render_webgl {
  constructor(canvas_dom, canvas_background_dom) {
    this.dom = canvas_dom;
    this.background_dom = canvas_background_dom;

    this.gl =
      this.dom.getContext("webgl") || this.dom.getContext("experimental-webgl");
    if (!this.gl instanceof WebGLRenderingContext) {
      throw new Error("No WebGL present");
    }
    this.shaderProgram = this.initShaderProgram(
      this.vertexShaderText,
      this.fragmentShaderText
    );
    if (!this.shaderProgram) {
      throw new Error("Unable to initialize the shader program");
    }
    this.loadVertex();
    this.gl.useProgram(this.shaderProgram);

    this.redrawCanvas();
    window.onresize = function () {
      this.redrawCanvas();
    };
  }

  setSpokes(spokes, max_spoke_len) {
    this.spokes = spokes;
    this.max_spoke_len = max_spoke_len;
    this.loadTexture();
  }

  setRange(r) {
    this.range = r;
  }

  setRangeControl(c) {
    this.rangeControl = c;
  }

  setLegend(l) {
    this.legend = l;
  }

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
    this.beam_length = Math.trunc(Math.max(this.center_x, this.center_y) * 0.9);
    this.background_ctx = this.background_dom.getContext("2d");

    this.drawRings();
  }

  drawRings() {
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
      if (i > 0 && this.range && this.rangeControl) {
        let r = Math.trunc((this.range * i) / 4);
        console.log("i=" + i + " range=" + this.range + " r=" + r);
        let text = this.rangeControl.descriptions[r]
          ? this.rangeControl.descriptions[r]
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
  }

  drawSpoke(spoke) {
    if (!this.data) {
      return;
    }
    for (
      let i = 0, idx = spoke.angle * this.max_spoke_len * 4;
      i < spoke.data.length;
      i++, idx += 4
    ) {
      let v = spoke.data[i];

      this.data[idx + 0] = this.legend[v][0];
      this.data[idx + 1] = this.legend[v][1];
      this.data[idx + 2] = this.legend[v][2];
      this.data[idx + 3] = this.legend[v][3];
    }
  }

  render() {
    if (!this.data) {
      return;
    }

    let gl = this.gl;

    // We tell the GPU to draw a square from (-x,-y) to (+x,+y)
    // The shader morphs this into a circle.

    /*============= Drawing the Quad ================*/

    this.updateTexture();

    // Clear the canvas
    gl.clearColor(0.1, 0.4, 0.4, 1.0);
    gl.enable(gl.DEPTH_TEST);
    gl.clear(gl.COLOR_BUFFER_BIT);

    // Set the view port
    gl.viewport(0, 0, this.width, this.height);

    const samplerLocation = gl.getUniformLocation(
      this.shaderProgram,
      "uSampler"
    );
    gl.uniform1i(samplerLocation, 0);

    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  vertexShaderText = `
    attribute vec4 aPosition;
    attribute vec2 aTexCoord;
    varying highp vec2 vTexCoord;
    void main(void) {
        gl_Position = aPosition;
        vTexCoord = aTexCoord;
    }
    `;

  fragmentShaderText = `
    precision highp float;

    varying highp vec2 vTexCoord;
    uniform sampler2D uSampler;
    void main()
    {
       float d = length(vTexCoord.xy);
       if (d >= 1.0) 
          discard;
       float a = atan(vTexCoord.y, vTexCoord.x) / 6.28318;
       gl_FragColor = texture2D(uSampler, vec2(d, a));
    }
    `;

  fragmentShaderTextSquare = `
    varying highp vec2 vTexCoord;
    uniform sampler2D uSampler;
    void main(void) {
        gl_FragColor = texture2D(uSampler, vTexCoord);
    }
    `;

  //
  // Initialize a shader program, so WebGL knows how to draw our data
  //
  initShaderProgram(vsSource, fsSource) {
    const vertexShader = this.loadShader(this.gl.VERTEX_SHADER, vsSource);
    const fragmentShader = this.loadShader(this.gl.FRAGMENT_SHADER, fsSource);

    // Create the shader program

    const shaderProgram = this.gl.createProgram();
    this.gl.attachShader(shaderProgram, vertexShader);
    this.gl.attachShader(shaderProgram, fragmentShader);
    this.gl.linkProgram(shaderProgram);

    // If creating the shader program failed, alert

    if (!this.gl.getProgramParameter(shaderProgram, this.gl.LINK_STATUS)) {
      throw new Error(
        `Unable to initialize the shader program: ${this.gl.getProgramInfoLog(
          shaderProgram
        )}`
      );
    }

    return shaderProgram;
  }

  //
  // creates a shader of the given type, uploads the source and
  // compiles it.
  //
  loadShader(type, source) {
    const shader = this.gl.createShader(type);

    // Send the source to the shader object

    this.gl.shaderSource(shader, source);

    // Compile the shader program

    this.gl.compileShader(shader);

    // See if it compiled successfully

    if (!this.gl.getShaderParameter(shader, this.gl.COMPILE_STATUS)) {
      throw new Error(
        `An error occurred compiling the shaders: ${this.gl.getShaderInfoLog(
          shader
        )}`
      );
      gl.deleteShader(shader);
      return null;
    }

    return shader;
  }

  loadTexture() {
    let gl = this.gl;
    if (this.max_spoke_len === undefined || this.spokes === undefined) {
      return;
    }
    let width = this.max_spoke_len;
    let height = this.spokes;

    const texture = gl.createTexture();
    this.data = new Uint8Array(width * height * 4);
    for (let i = 0; i < this.data.length; i++) {
      this.data[i * 4] = i & 255;
      this.data[i * 4 + 3] = 255;
    }
    gl.bindTexture(gl.TEXTURE_2D, texture);

    this.updateTexture();
  }

  updateTexture() {
    let gl = this.gl;

    if (this.max_spoke_len === undefined || this.spokes === undefined) {
      return;
    }

    gl.texImage2D(
      gl.TEXTURE_2D,
      0,
      gl.RGBA,
      this.max_spoke_len,
      this.spokes,
      0,
      gl.RGBA,
      gl.UNSIGNED_BYTE,
      this.data
    );
    gl.generateMipmap(gl.TEXTURE_2D);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MIN_FILTER, gl.GL_LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  loadVertex() {
    let gl = this.gl;

    const positionBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, positionBuffer);
    const positions = [-1.0, 1.0, 1.0, 1.0, -1.0, -1.0, 1.0, -1.0];
    // const positions = [-0.9, 0.9, 0.9, 0.9, -0.9, -0.9, 0.9, -0.9];
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(positions), gl.STATIC_DRAW);
    const positionLocation = gl.getAttribLocation(
      this.shaderProgram,
      "aPosition"
    );
    gl.bindBuffer(gl.ARRAY_BUFFER, positionBuffer);
    gl.vertexAttribPointer(positionLocation, 2, gl.FLOAT, false, 0, 0);
    gl.enableVertexAttribArray(positionLocation);

    const texCoordBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, texCoordBuffer);
    const texCoords = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    //const texCoords = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(texCoords), gl.STATIC_DRAW);
    const texCoordLocation = gl.getAttribLocation(
      this.shaderProgram,
      "aTexCoord"
    );
    gl.bindBuffer(gl.ARRAY_BUFFER, texCoordBuffer);
    gl.vertexAttribPointer(texCoordLocation, 2, gl.FLOAT, false, 0, 0);
    gl.enableVertexAttribArray(texCoordLocation);
  }
}
