"use strict";

export { RANGE_SCALE };

import {
  loadRadar,
  registerRadarCallback,
  registerRangeCallback,
} from "./control.js";
import "./protobuf/protobuf.js";

const prefix = "myr_";

import { render_2d } from "./render_2d.js";
import { render_webgl } from "./render_webgl.js";
import { render_webgl_alt } from "./render_webgl_alt.js";

var webSocket;
var RadarMessage;
var renderer;

const RANGE_SCALE = 0.9; // Factor by which we fill the (w,h) canvas with the outer radar range ring

registerRadarCallback(radarLoaded);
registerRangeCallback(rangeUpdate);

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
        document.getElementById("myr_canvas_background")
      );
    } else if (draw == "alt") {
      renderer = new render_webgl_alt(
        document.getElementById("myr_canvas_webgl"),
        document.getElementById("myr_canvas_background")
      );
    } else {
      renderer = new render_webgl(
        document.getElementById("myr_canvas_webgl"),
        document.getElementById("myr_canvas_background")
      );
    }
  } catch (e) {
    console.log(e);
    console.log("Falling back on 2d context");
    renderer = new render_2d(
      document.getElementById("myr_canvas"),
      document.getElementById("myr_canvas_background")
    );
  }

  loadRadar(id);

  window.onresize = function () {
    renderer.redrawCanvas();
  };
};

function restart(id) {
  setTimeout(loadRadar(id), 5000);
}

function radarLoaded(r) {
  if (r === undefined || r.controls === undefined) {
    return;
  }
  renderer.setLegend(expandLegend(r.legend));
  renderer.setSpokes(r.spokes, r.maxSpokeLen);

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
          renderer.drawSpoke(message.spokes[i]);
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

function rangeUpdate(range, descriptions) {
  renderer.setRange(range, descriptions);
}
