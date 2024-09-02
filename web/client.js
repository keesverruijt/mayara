import van from "./van-1.5.2.debug.js";

const {div, label, input} = van.tags

var radar;
var controls;

const StringValue = (id) =>
  div({class: 'control'},
    label({ for: id }, id),
    input({ type: 'text', id: id, size: 20, readonly: true })
  )

function radarsLoaded(id, d) {
  radar = d[id];

  if (radar === null || radar.controls === null) {
    setTimeout(loadRadars(id), 15000);
    return;
  }
  controls = radar.controls;
  buildControls();
}

function buildControls() {
  let c = document.getElementById('controls');
  c.innerHTML = "";
  van.add(c, div(radar.name + " Controls"));

  for (const [k, v] of Object.entries(controls)) {
    if ('isStringValue' in v) {
      van.add(c, StringValue(k));
    }
  }
  console.log(controls);

  
}

function loadRadars(id) {
  fetch('/v1/api/radars')
  .then(res => res.json())
  .then(out => radarsLoaded(id, out))
  .catch(err => setTimeout(loadRadars(id), 15000));
}

window.onload = function () {
  const urlParams = new URLSearchParams(window.location.search);
  const id = urlParams.get('id');

  if (id !== null) {
    setTimeout(loadRadars(id), 200);
  }
}
