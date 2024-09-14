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
    input({ type: 'number', id: id, onchange: e => change(e), oninput: e => do_input(e) })
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

  loadRadars(id);
}

function loadRadars(id) {
  fetch('/v1/api/radars')
  .then(res => res.json())
  .then(out => radarsLoaded(id, out))
  .catch(err => restart(id));
}

function restart(id) {
  setTimeout(loadRadars(id), 15000);
}
function radarsLoaded(id, d) {
  radar = d[id];

  if (radar === undefined || radar.controls === undefined) {
    restart(id);
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
    restart(id);
  }
  webSocket.onmessage = (e) => {
    
    let v = JSON.parse(e.data);
    let i = document.getElementById(v.id);
    i.setAttribute('value', v.value);
    console.log("<- " + e.data + " = " + controls[v.id].name);
    if ('descriptions' in controls[v.id] && i.type == 'range') {
      let description = controls[v.id].descriptions[v.value];
      let d = i.parentNode.querySelector('.description');
      if (d) d.innerHTML = description;
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

function do_input(e) {
  let v = e.target;
  console.log("input " + e + " " + v.id + "=" + v.value);
}