
import { loadRadar, registerRadarCallback }  from "./control.js";
import "./proto/protobuf.js";

const prefix = 'myr_';

var radar;
var webSocket;
var RadarMessage;
var canvas;
var canvas;

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

function redrawCanvas() {
  canvas.width = canvas.dom.width;
  canvas.height = canvas.dom.height;
  canvas.center_x = canvas.width / 2;
  canvas.center_y = canvas.height / 2;
  canvas.ctx = canvas.dom.getContext("2d");

  drawRings();
}

function drawRings() {
  canvas.ctx.strokeStyle = "#FFFFFF";
  canvas.ctx.beginPath();

  for (let i = 0; i <= canvas.center_x; i = i + 50) {
    canvas.ctx.arc(canvas.center_x, canvas.center_y, i, 0, 2 * Math.PI);
  }
  
  canvas.ctx.stroke();
}

function drawSpoke(spoke) {
  let s = spoke;
}