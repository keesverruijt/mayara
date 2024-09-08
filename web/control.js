import van from "./van-1.5.2.debug.js";

const {div, label, input} = van.tags

var radar;
var controls;
var webSocket;

const StringValue = (id) =>
  div({class: 'control'},
    label({ for: id }, id),
    input({ type: 'text', id: id, size: 20, readonly: true })
  )

const NumericValue = (id) =>
  div({class: 'control'},
    label({ for: id }, id),
    input({ type: 'number', id: id, readonly: false })
    )
  
window.onload = function () {
  const urlParams = new URLSearchParams(window.location.search);
  const id = urlParams.get('id');

  if (id !== null) {
    setTimeout(loadRadars(id), 200);
  }
}

function loadRadars(id) {
  fetch('/v1/api/radars')
  .then(res => res.json())
  .then(out => radarsLoaded(id, out))
  .catch(err => setTimeout(loadRadars(id), 15000));
}

function radarsLoaded(id, d) {
  radar = d[id];

  if (radar === undefined || radar.controls === undefined) {
    setTimeout(loadRadars(id), 15000);
    return;
  }
  controls = radar.controls;
  buildControls();

  webSocket = new WebSocket(radar.controlUrl);

  webSocket.onopen = (e) => {
    console.log("websocket open: " + JSON.stringify(e));
  }
  webSocket.onclose = (e) => {
    console.log("websocket close: " + e);
  }
  webSocket.onmessage = (e) => {
    console.log("websocket message: " + e.data);
    let v = JSON.parse(e.data);
    let d = document.getElementById(v.id);
    if (d) {
      if ('stringValue' in v) {
        d.setAttribute('value', v.stringValue);
      } else if ('description' in v) {
        d.setAttribute('value', v.stringValue);
      } else if ('value' in v) {
        d.setAttribute('value', v.stringValue);
      } 
    }
  }

}

function buildControls() {
  let c = document.getElementById('controls');
  c.innerHTML = "";
  van.add(c, div(radar.name + " Controls"));

  for (const [k, v] of Object.entries(controls)) {
    if ('isStringValue' in v) {
      van.add(c, StringValue(k));
    } else {
      van.add(c, NumericValue(k));
    }
  }
  console.log(controls);
}
