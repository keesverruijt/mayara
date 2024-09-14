import van from "./van-1.5.2.debug.js";

const {div, label, input, button} = van.tags

var radar;
var controls;
var webSocket;

const StringValue = (id, name) =>
  div({class: 'control'},
    label({ for: id }, name),
    input({ type: 'text', id: id, size: 20 })
  )

const NumericValue = (id, name) =>
  div({class: 'control'},
    label({ for: id }, name),
    input({ type: 'number', id: id, onchange: e => change(e) })
  )
    
const RangeValue = (id, name, min, max, def, descriptions) =>
  div({ class: 'control' },
    label({ for: id }, name),
    (descriptions) ? div({ class: 'description' }) : null,
    input({ type: 'range', id, min, max, value: def, onchange: e => change(e)})
  )
  
const SetButton = () => button({ onclick: e => set_button(e) }, 'Set')
 
  

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
    d.setAttribute('value', ('description' in v && d.type == 'text') ? v.description : v.value);
    if ('descriptions' in controls[v.id] && d.type == 'range') {
      let desc = d.parentNode.querySelector('.description');
      if (desc) desc.innerHTML = v.description;
    }
  }

}

function buildControls() {
  let c = document.getElementById('controls');
  c.innerHTML = "";
  van.add(c, div(radar.name + " Controls"));

  for (const [k, v] of Object.entries(controls)) {
    van.add(c, (v['isStringValue'])
      ? StringValue(k, v.name)
      : ('maxValue' in v && v.maxValue <= 100)
        ? RangeValue(k, v.name, v.minValue, v.maxValue, 0, 'descriptions' in v)
        : NumericValue(k, v.name));
    if (v['isReadOnly']) {
      document.getElementById(k).setAttribute('readonly', 'true');
    } else if (v['isStringValue']) {
      van.add(document.getElementById(k).parentNode, SetButton());
    }
  }
  console.log(controls);
}

function change(e) {
  let v = e.target;
  console.log("change " + e + " " + v.id + "=" + v.value);
  let cv = JSON.stringify({ id: v.id, value: v.value });
  webSocket.send(cv);
  console.log(controls[v.id].name + "-> " + cv);
}

function set_button(e) {
  let v = e.target.previousElementSibling;
  console.log("set_button " + e + " " + v.id + "=" + v.value);
  let cv = JSON.stringify({ id: v.id, value: v.value });
  webSocket.send(cv);
  console.log(controls[v.id].name + "-> " + cv);
}