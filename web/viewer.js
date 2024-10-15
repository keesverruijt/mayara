
import { loadRadar, registerRadarCallback, registerRangeCallback }  from "./control.js";
import "./protobuf/protobuf.js";

const prefix = 'myr_';

var radar;
var webSocket;
var RadarMessage;
var canvas;
var rgbaLegend;
var myr_range_control;
var myr_range;

registerRadarCallback(radarLoaded);
registerRangeCallback(rangeUpdate);

window.onload = function () {
  const urlParams = new URLSearchParams(window.location.search);
  const id = urlParams.get('id');

  loadRadar(id);

  protobuf.load("./proto/RadarMessage.proto", function (err, root) {
    if (err)
      throw err;

    RadarMessage = root.lookupType(".RadarMessage");
  });

  canvas = Object;
  canvas.dom = document.getElementById('myr_canvas');
  canvas.background_dom = document.getElementById('myr_canvas_background');
  redrawCanvas();
  window.onresize = function(){ redrawCanvas(); }
}

function restart(id) {
  setTimeout(loadRadar(id), 5000);
}


function radarLoaded(r) {
  radar = r;

  if (radar === undefined || radar.controls === undefined) {
    return;
  }
  expandLegend();

  webSocket = new WebSocket(radar.streamUrl);
  webSocket.binaryType = "arraybuffer";

  webSocket.onopen = (e) => {
    console.log("websocket open: " + JSON.stringify(e));
  }
  webSocket.onclose = (e) => {
    console.log("websocket close: " + e);
    restart(radar.id);
  }
  webSocket.onmessage = (e) => {
    if (RadarMessage) {
      let buf = e.data;
      let bytes = new Uint8Array(buf);
      var message = RadarMessage.decode(bytes);
      if (message.spokes) {
        for (let i = 0; i < message.spokes.length; i++) {
          drawSpoke(message.spokes[i]);
        }
      }
    }
  }
}

function expandLegend() {
  let legend = radar.legend;
  let a = Array();
  for (let i = 0; i < Object.keys(legend).length; i++) {
    let color = legend[i].color;
    a.push(hexToRGBA(color));
  }
  a[0][3] = 255;
  
  rgbaLegend = a;
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

function redrawCanvas() {
  var parent = canvas.dom.parentNode,
    styles = getComputedStyle(parent),
    w = parseInt(styles.getPropertyValue("width"), 10),
    h = parseInt(styles.getPropertyValue("height"), 10);

  canvas.dom.width = w;
  canvas.dom.height = h;
  canvas.background_dom.width = w;
  canvas.background_dom.height = h;

  canvas.width = canvas.dom.width;
  canvas.height = canvas.dom.height;
  canvas.center_x = canvas.width / 2;
  canvas.center_y = canvas.height / 2;
  canvas.beam_length = Math.trunc(Math.max(canvas.center_x, canvas.center_y) * 0.9);
  canvas.ctx = canvas.dom.getContext("2d", { alpha: true });
  canvas.background_ctx = canvas.background_dom.getContext("2d");
  
  canvas.pattern = document.createElement('canvas');
  canvas.pattern.width = 2048;
  canvas.pattern.height = 1;
  canvas.pattern_ctx = canvas.pattern.getContext('2d');
  canvas.image = canvas.pattern_ctx.createImageData(2048, 1);
  
  drawRings();
}

function rangeUpdate(control, range) {
  myr_range_control = control;
  myr_range = range;
  drawRings();
}

function drawRings() {
  canvas.background_ctx.setTransform(1, 0, 0, 1, 0, 0);
  canvas.background_ctx.clearRect(0, 0, canvas.width, canvas.height);
  
  canvas.background_ctx.strokeStyle = "white";
  canvas.background_ctx.fillStyle = "white";
  canvas.background_ctx.font = "bold 16px/1 Verdana, Geneva, sans-serif";
  for (let i = 0; i <= 4; i++) {
    canvas.background_ctx.beginPath();
    canvas.background_ctx.arc(canvas.center_x, canvas.center_y, i * canvas.beam_length / 4, 0, 2 * Math.PI);
    canvas.background_ctx.stroke();
    if (i > 0 && myr_range && myr_range_control) {
      let r = Math.trunc(myr_range * i / 4);
      console.log("i=" + i + " range=" + myr_range + " r=" + r);
      let text = (myr_range_control.descriptions[r]) ? myr_range_control.descriptions[r] : undefined;
      if (text === undefined) {
        if (r % 1000 == 0) {
          text = (r / 1000) + " km";
        }
        else {
          text = r + " m";
        }
      }
      canvas.background_ctx.fillText(text, canvas.center_x + i * canvas.beam_length * 1.41 / 8, canvas.center_y + i * canvas.beam_length * -1.41 / 8);
    }
  }
  
  canvas.background_ctx.fillStyle = "lightblue";
  canvas.background_ctx.fillText("MAYARA", 5, 20);
}

function drawSpoke(spoke) {
  let a = 2 * Math.PI * ((spoke.angle + radar.spokes * 3 / 4) % radar.spokes) / radar.spokes;
  let pixels_per_item = canvas.beam_length * 0.9 / spoke.data.length;
  if (myr_range) {
    pixels_per_item = pixels_per_item * spoke.range / myr_range;
  }
  let c = Math.cos(a) * pixels_per_item;
  let s = Math.sin(a) * pixels_per_item;
 
  for (let i = 0, idx = 0; i < spoke.data.length; i++, idx += 4) {
    let v = spoke.data[i];
    
    canvas.image.data[idx + 0] = rgbaLegend[v][0];
    canvas.image.data[idx + 1] = rgbaLegend[v][1];
    canvas.image.data[idx + 2] = rgbaLegend[v][2];
    canvas.image.data[idx + 3] = rgbaLegend[v][3];
  }

  canvas.pattern_ctx.putImageData(canvas.image, 0, 0);
 
  let pattern = canvas.ctx.createPattern(canvas.pattern, "repeat-x");

  let arc_angle = 2 * Math.PI / radar.spokes;

  canvas.ctx.setTransform(c, s, -s, c, canvas.center_x, canvas.center_y);
  canvas.ctx.fillStyle = pattern;
  canvas.ctx.beginPath();
  canvas.ctx.moveTo(0, 0);
  canvas.ctx.arc(0, 0, spoke.data.length, 0, arc_angle);
  canvas.ctx.closePath();
  canvas.ctx.fill();
  
 }
