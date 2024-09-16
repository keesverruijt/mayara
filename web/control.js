import van from "./van-1.5.2.debug.js";

const {div, label, input, button, select, option} = van.tags

var radar;
var controls;
var webSocket;
var error_message;

const StringValue = (id, name) =>
  div({class: 'control'},
    label({ for: id }, name),
    input({ type: 'text', id: id, size: 20 })
  )

const NumericValue = (id, name) =>
  div({class: 'control'},
    label({ for: id }, name),
    div({ class: 'numeric' }),
    input({ type: 'number', id: id, onchange: e => do_change(e), oninput: e => do_input(e) })
  )
    
const RangeValue = (id, name, min, max, def, descriptions) =>
  div({ class: 'control' },
    label({ for: id }, name),
    div({ class: 'description' }),
    input({ type: 'range', id, min, max, value: def, onchange: e => do_change(e)})
  )
  
const SelectValue = (id, name, validValues, descriptions) => {
  let r =
    div({ class: 'control' },
      label({ for: id }, name),
      div({ class: 'description' }),
      select({ id, onchange: e => do_change(e) }, validValues.map(v => option({ value: v }, descriptions[v]))
      )
    );
  return r;
}

const SetButton = () => button({ onclick: e => do_button(e) }, 'Set')
 
class TemporaryMessage {
  timeoutId;
  element;

  constructor(id) {
    this.element = document.getElementById(id);
  }

  raise(aMessage) {
    this.element.style.visibility = 'visible';
    this.element.innerHTML = aMessage;
    this.timeoutId = setTimeout(() => { this.cancel(); }, 5000);
  };

  cancel() {
    if (typeof this.timeoutId === "number") {
      clearTimeout(this.timeoutId);
    }
    this.element.style.visibility = 'hidden';
  }
};

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
  error_message = new TemporaryMessage('error');
  
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
    let control = controls[v.id];

    i.value = v.value;
    console.log("<- " + e.data + " = " + control.name + " = " + i.value);
    let n = i.parentNode.querySelector('.numeric');
    if (n) n.innerHTML = v.value;
    let d = i.parentNode.querySelector('.description');
    if (d) {
      let description = (control.descriptions) ? control.descriptions[v.value] : undefined;
      if (!description) description = v.value;
      d.innerHTML = description;
    }
    if (v.error) {
      error_message.raise(v.error);
    }
  }
}

function buildControls() {
  let c = document.getElementById('title');
  c.innerHTML = "";
  van.add(c, div(radar.name + " Controls"));

  c = document.getElementById('controls');
  for (const [k, v] of Object.entries(controls)) {
    van.add(c, (v['isStringValue'])
      ? StringValue(k, v.name)
      : ('validValues' in v)
        ? SelectValue(k, v.name, v['validValues'], v['descriptions'])
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

function do_change(e) {
  let v = e.target;
  console.log("change " + e + " " + v.id + "=" + v.value);
  let cv = JSON.stringify({ id: v.id, value: v.value });
  webSocket.send(cv);
  console.log(controls[v.id].name + "-> " + cv);
}

function do_button(e) {
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
