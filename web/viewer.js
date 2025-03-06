"use strict";

export { RANGE_SCALE };

import {
  loadRadar,
  registerRadarCallback,
  registerControlCallback,
} from "./control.js";
import "./protobuf/protobuf.min.js";

const prefix = "myr_";

import { render_2d } from "./render_2d.js";
import { render_webgl } from "./render_webgl.js";
import { render_webgl_alt } from "./render_webgl_alt.js";

var webSocket;
var RadarMessage;
var renderer;
var rangeDescriptions;
var noTransmitAngles;

// Fill rangeDescriptions so that if we do replay of the demo, we show NM instead of m.
rangeDescriptions = {
  231: "1/8 nm",
  463: "1/4 nm",
  926: "1/2 nm",
  1389: "3/4 nm",
  1852: "1 nm",
  3704: "2 nm",
  5556: "3 nm",
  7408: "4 nm",
  9260: "5 nm",
  11112: "6 nm",
  14816: "8 nm",
  18520: "10 nm",
  22224: "12 nm",
  27780: "15 nm",
  29632: "16 nm",
  37040: "20 nm",
  44448: "24 nm",
};

const RANGE_SCALE = 0.9; // Factor by which we fill the (w,h) canvas with the outer radar range ring

registerRadarCallback(radarLoaded);
registerControlCallback(controlUpdate);

window.onload = function () {
  const urlParams = new URLSearchParams(window.location.search);
  const id = urlParams.get("id");
  const draw = urlParams.get("draw");

  protobuf.load("./proto/RadarMessage.proto", function (err, root) {
    if (err) throw err;

    RadarMessage = root.lookupType(".RadarMessage");
  });

  try {
    if (draw == "2d") {
      renderer = new render_2d(
        document.getElementById("myr_canvas"),
        document.getElementById("myr_canvas_background"),
        drawBackground
      );
    } else if (draw == "alt") {
      renderer = new render_webgl_alt(
        document.getElementById("myr_canvas_webgl"),
        document.getElementById("myr_canvas_background"),
        drawBackground
      );
    } else {
      renderer = new render_webgl(
        document.getElementById("myr_canvas_webgl"),
        document.getElementById("myr_canvas_background"),
        drawBackground
      );
    }
  } catch (e) {
    console.log(e);
    console.log("Falling back on 2d context");
    renderer = new render_2d(
      document.getElementById("myr_canvas"),
      document.getElementById("myr_canvas_background"),
      drawBackground
    );
  }

  loadRadar(id);

  window.onresize = function () {
    renderer.redrawCanvas();
  };
};

function restart(id) {
  setTimeout(loadRadar, 15000, id);
}

function radarLoaded(r) {
  let maxSpokeLen = r.maxSpokeLen;
  let spokes = r.spokes;
  let prev_angle = -1;

  if (r === undefined || r.controls === undefined) {
    return;
  }
  renderer.setLegend(expandLegend(r.legend));
  renderer.setSpokes(spokes, maxSpokeLen);

  webSocket = new WebSocket(r.streamUrl);
  webSocket.binaryType = "arraybuffer";

  webSocket.onopen = (e) => {
    console.log("websocket open: " + JSON.stringify(e));
  };
  webSocket.onclose = (e) => {
    console.log("websocket close: " + e);
    restart(r.id);
  };
  webSocket.onmessage = (e) => {
    if (RadarMessage) {
      let buf = e.data;
      let bytes = new Uint8Array(buf);
      var message = RadarMessage.decode(bytes);
      if (message.spokes) {
        for (let i = 0; i < message.spokes.length; i++) {
          let spoke = message.spokes[i];

          // The number of spokes actually sent is usually lower than the stated angles,
          // fill out the spokes between prev_angle and spoke.angle by repeating the spoke X times.
          if (prev_angle > -1) {
            let new_angle = spoke.angle;
            if (prev_angle > new_angle) {
              for (let angle = prev_angle; angle < spokes; angle++) {
                spoke.angle = angle;
                renderer.drawSpoke(spoke);
              }
              prev_angle = 0;
            }
            for (let angle = prev_angle; angle < new_angle; angle++) {
              spoke.angle = angle;
              renderer.drawSpoke(spoke);
            }
            spoke.angle = new_angle;
          }
          renderer.drawSpoke(spoke);
          prev_angle = spoke.angle + 1;
        }
        renderer.render();
      }
    }
  };
}

function expandLegend(legend) {
  let a = Array();
  for (let i = 0; i < Object.keys(legend).length; i++) {
    let color = legend[i].color;
    a.push(hexToRGBA(color));
  }
  a[0][3] = 255;

  return a;
}

function hexToRGBA(hex) {
  let a = Array();
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

function controlUpdate(control, controlValue) {
  if (control.name == "Range") {
    let range = parseFloat(controlValue.value);
    if (controlValue.descriptions) {
      rangeDescriptions = control.descriptions;
    }
    renderer.setRange(range);
  }
  if (control.name.startsWith("No Transmit")) {
    let value = parseFloat(controlValue.value);
    let idx = extractNoTxZone(control.name);
    let start_or_end = extractStartOrEnd(control.name);
    if (controlValue.enabled) {
      noTransmitAngles[idx][start_or_end] = value;
    } else {
      noTransmitAngles[idx] = null;
    }
  }
}

function extractNoTxZone(name) {
  const re = /(\d+)/;
  let match = name.match(re);
  if (match) {
    return parseInt(match[1]);
  }
  return 0;
}

function extractStartOrEnd(name) {
  return name.includes("start") ? 0 : 1;
}

function drawBackground(obj, txt) {
  obj.background_ctx.setTransform(1, 0, 0, 1, 0, 0);
  obj.background_ctx.clearRect(0, 0, obj.width, obj.height);

  obj.background_ctx.strokeStyle = "white";
  obj.background_ctx.fillStyle = "white";
  obj.background_ctx.font = "bold 16px/1 Verdana, Geneva, sans-serif";
  for (let i = 0; i <= 4; i++) {
    obj.background_ctx.beginPath();
    obj.background_ctx.arc(
      obj.center_x,
      obj.center_y,
      (i * obj.beam_length) / 4,
      0,
      2 * Math.PI
    );
    obj.background_ctx.stroke();
    if (i > 0 && obj.range) {
      let r = Math.trunc((obj.range * i) / 4);
      let text = rangeDescriptions ? rangeDescriptions[r] : undefined;
      if (text === undefined) {
        if (r % 1000 == 0) {
          text = r / 1000 + " km";
        } else {
          text = r + " m";
        }
      }
      obj.background_ctx.fillText(
        text,
        obj.center_x + (i * obj.beam_length * 1.41) / 8,
        obj.center_y + (i * obj.beam_length * -1.41) / 8
      );
    }
  }

  obj.background_ctx.fillStyle = "lightgrey";

  if (typeof noTransmitAngles == "array") {
    noTransmitAngles.forEach((e) => {
      if (e && e[0]) {
        obj.background_ctx.beginPath();
        obj.background_ctx.arc(
          obj.center_x,
          obj.center_y,
          obj.beam_length * 2,
          (2 * Math.PI * e[0]) / obj.spokes,
          (2 * Math.PI * e[1]) / obj.spokes
        );
        obj.background_ctx.fill();
      }
    });
  }
  obj.background_ctx.fillStyle = "lightblue";
  this.background_ctx.fillText(txt, 5, 20);
}
