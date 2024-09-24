
import { loadRadar, registerRadarCallback }  from "./control.js";
import "./proto/protobuf.js";

const prefix = 'myr_';

var radar;
var webSocket;
var RadarMessage;
var canvas;
var rgbaLegend;

registerRadarCallback(radarLoaded);

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
  var parent = canvas.dom.parentNode,
    styles = getComputedStyle(parent),
    w = parseInt(styles.getPropertyValue("width"), 10),
    h = parseInt(styles.getPropertyValue("height"), 10);

  canvas.dom.width = w;
  canvas.dom.height = h;
  redrawCanvas();
}

function radarLoaded(r) {
  radar = r;

  if (radar === undefined || radar.controls === undefined) {
    return;
  }
  expandLegend();

  webSocket = new WebSocket(radar.streamUrl);

  webSocket.onopen = (e) => {
    console.log("websocket open: " + JSON.stringify(e));
  }
  webSocket.onclose = (e) => {
    console.log("websocket close: " + e);
    setControl({ id: '0', value: '0' });
    restart(id);
  }
  webSocket.onmessage = (e) => {
    if ('bytes' in e.data) {
      if (RadarMessage) {
        e.data.bytes().then((a) => {
          var message = RadarMessage.decode(a);
          if (message.spokes) {
            for (let i = 0; i < message.spokes.length; i++) {
              drawSpoke(message.spokes[i]);
            }
          }
        });
        
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
  canvas.width = canvas.dom.width;
  canvas.height = canvas.dom.height;
  canvas.center_x = canvas.width / 2;
  canvas.center_y = canvas.height / 2;
  canvas.beam_length = Math.max(canvas.center_x, canvas.center_y);
  canvas.ctx = canvas.dom.getContext("2d", { alpha: true });
  canvas.background_ctx = canvas.background_dom.getContext("2d");
  
  canvas.pattern = document.createElement('canvas');
  canvas.pattern.width = 2048;
  canvas.pattern.height = 1;
  canvas.pattern_ctx = canvas.pattern.getContext('2d');
  canvas.image = canvas.pattern_ctx.createImageData(2048, 1);
  
  drawRings();
}

function drawRings() {
  canvas.background_ctx.strokeStyle = "#FFFFFFFF";
  canvas.background_ctx.beginPath();

  canvas.background_ctx.setTransform(1, 0, 0, 1, canvas.center_x, canvas.center_y);
  canvas.background_ctx.beginPath();
  for (let i = 50; i <= canvas.beam_length; i = i + 50) {
    canvas.background_ctx.arc(0, 0, i, 0, 2 * Math.PI);
  }
  canvas.background_ctx.stroke();

  canvas.background_ctx.setTransform(1, 0, 0, 1, 0, 0);
  canvas.background_ctx.fillStyle = "lightblue";
  canvas.background_ctx.font = "bold 18px sans";
  canvas.background_ctx.fillText("MAYARA", 5, 20);

}

var f;

function drawSpoke(spoke) {
  //if (spoke.angle < 0 || spoke.angle > 2) return;
  let a = 2 * Math.PI * ((spoke.angle + radar.spokes / 2) % radar.spokes) / radar.spokes;
  let r = spoke.range;
  let pixels_per_item = canvas.beam_length * 1.0 / spoke.data.length;
  let c = Math.cos(a) * pixels_per_item;
  let s = Math.sin(a) * pixels_per_item;
  // let c = 1 * pixels_per_item;
  // let s = 0 * pixels_per_item;
 
  for (let i = 0, idx = 0; i < spoke.data.length; i++, idx += 4) {
    let v = spoke.data[i];
    
    canvas.image.data[idx + 0] = rgbaLegend[v][0];
    canvas.image.data[idx + 1] = rgbaLegend[v][1];
    canvas.image.data[idx + 2] = rgbaLegend[v][2];
    canvas.image.data[idx + 3] = 255;
    if (v > 0) {
      f++;
    }
  }
  if (a == 0) {
    console.log("spokes with data = " + f);
    f = 0;
  }

  canvas.pattern_ctx.putImageData(canvas.image, 0, 0);
 
  let pattern = canvas.ctx.createPattern(canvas.pattern, "repeat-x");

  let arc_angle = 2 * Math.PI / radar.spokes;

  canvas.ctx.setTransform(c, s, -s, c, canvas.center_x, canvas.center_y);
  canvas.ctx.fillStyle = pattern;
  canvas.ctx.strokeStyle = pattern;
  canvas.ctx.beginPath();
  canvas.ctx.moveTo(0, 0);
  canvas.ctx.arc(0, 0, spoke.data.length, 0, arc_angle);
  canvas.ctx.closePath();
  canvas.ctx.fill();
  // canvas.ctx.stroke();
  
  /*
  canvas.ctx.strokeStyle = "#000000FF";
  canvas.ctx.fillStyle = "#000000FF";
  canvas.ctx.moveTo(0, 0);
  canvas.ctx.arc(0, 0, spoke.data.length, arc_angle * 2, arc_angle * 4);
  canvas.ctx.fill();
  canvas.ctx.stroke();
  */
 }