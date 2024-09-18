export { loadRadar, registerRadarCallback };

import van from "./van-1.5.2.js";

const { div, label, input, button, select, option } = van.tags

const prefix = 'myr_';

var myr_radar;
var myr_controls;
var myr_webSocket;
var myr_error_message;
var myr_no_response_timeout;
var callbacks = Array();

function registerRadarCallback(callback) {
  callbacks.push(callback);
}

const StringValue = (id, name) =>
  div({class: 'myr_control'},
    label({ for: prefix + id }, name),
    input({ type: 'text', id: prefix + id, size: 20 })
  )

const NumericValue = (id, name) =>
  div({class: 'myr_control'},
    label({ for: prefix + id }, name),
    div({ class: 'myr_numeric' }),
    input({ type: 'number', id: prefix + id, onchange: e => do_change(e), oninput: e => do_input(e) })
  )
    
const RangeValue = (id, name, min, max, def, descriptions) =>
  div({ class: 'myr_control' },
    label({ for: prefix + id }, name),
    div({ class: 'myr_description' }),
    input({ type: 'range', id: prefix + id, min, max, value: def, onchange: e => do_change(e)})
  )
  
const SelectValue = (id, name, validValues, descriptions) => {
  let r =
    div({ class: 'myr_control' },
      label({ for: prefix + id }, name),
      div({ class: 'myr_description' }),
      select(
        { id: prefix + id, onchange: e => do_change(e) },
        validValues.map(v => option({ value: v }, descriptions[v]))
      )
    );
  return r;
}

const SetButton = () => button({ onclick: e => do_button(e) }, 'Set')
 
class TemporaryMessage {
  timeoutId;
  element;

  constructor(id) {
    this.element = get_element(id);
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

class Timeout {
  timeoutId;
  element;

  constructor(id) {
    this.element = get_element(id);
  }

  setTimeout() {
    this.cancel();
    this.timeoutId = setTimeout(() => { setControl({ id: '0', value: '0' }); }, 15000);
  };

  cancel() {
    if (typeof this.timeoutId === "number") {
      clearTimeout(this.timeoutId);
      this.timeoutId = undefined;
    }
  }
};

//
// This is not called when used in a nested module.
//
window.onload = function () {
  const urlParams = new URLSearchParams(window.location.search);
  const id = urlParams.get('id');

  loadRadar(id);
}

function loadRadar(id) {
  fetch('/v1/api/radars')
  .then(res => res.json())
  .then(out => radarsLoaded(id, out))
  .catch(err => restart(id));
}

function restart(id) {
  setTimeout(loadRadar(id), 15000);
}
function radarsLoaded(id, d) {
  myr_radar = d[id];

  if (myr_radar === undefined || myr_radar.controls === undefined) {
    restart(id);
    return;
  }
  myr_controls = myr_radar.controls;
  myr_error_message = new TemporaryMessage('error');
  
  buildControls();
  myr_no_response_timeout = new Timeout('0');

  myr_webSocket = new WebSocket(myr_radar.controlUrl);

  myr_webSocket.onopen = (e) => {
    console.log("websocket open: " + JSON.stringify(e));
  }
  myr_webSocket.onclose = (e) => {
    console.log("websocket close: " + e);
    setControl({ id: '0', value: '0' });
    restart(id);
  }
  myr_webSocket.onmessage = (e) => {
    let v = JSON.parse(e.data);
    setControl(v);
    myr_no_response_timeout.setTimeout();
  }

  callbacks.forEach((value) => {
    let r = myr_radar;
    value(r);
  });
}

function setControl(v) {
  let i = get_element(v.id);
  let control = myr_controls[v.id];
  if (i && control) {
    i.value = v.value;
    console.log("<- " + control.name + " = " + v.value);
    let n = i.parentNode.querySelector('.myr_numeric');
    if (n) n.innerHTML = v.value;
    let d = i.parentNode.querySelector('.myr_description');
    if (d) {
      let description = (control.descriptions) ? control.descriptions[v.value] : undefined;
      if (!description) description = v.value;
      d.innerHTML = description;
    }
    if (v.error) {
      myr_error_message.raise(v.error);
    }
  }
}

function buildControls() {
  let c = get_element('title');
  c.innerHTML = "";
  van.add(c, div(myr_radar.name + " Controls"));

  c = get_element('controls');
  c.innerHTML = "";
  for (const [k, v] of Object.entries(myr_controls)) {
    van.add(c, (v['isStringValue'])
      ? StringValue(k, v.name)
      : ('validValues' in v)
        ? SelectValue(k, v.name, v['validValues'], v['descriptions'])
          : ('maxValue' in v && v.maxValue <= 100)
          ? RangeValue(k, v.name, v.minValue, v.maxValue, 0, 'descriptions' in v)
          : NumericValue(k, v.name));
    if (v['isReadOnly']) {
      get_element(k).setAttribute('readonly', 'true');
    } else if (v['isStringValue']) {
      van.add(get_element(k).parentNode, SetButton());
    }
  }
}

function do_change(e) {
  let v = e.target;
  let id = strip_prefix(v.id);
  console.log("change " + e + " " + id + "=" + v.value);
  let cv = JSON.stringify({ id: id, value: v.value });
  myr_webSocket.send(cv);
  console.log(myr_controls[id].name + "-> " + cv);
}

function do_button(e) {
  let v = e.target.previousElementSibling;
  let id = strip_prefix(v.id);
  console.log("set_button " + e + " " + id + "=" + v.value);
  let cv = JSON.stringify({ id: id, value: v.value });
  myr_webSocket.send(cv);
  console.log(myr_controls[id].name + "-> " + cv);
}

function do_input(e) {
  let v = e.target;
  console.log("input " + e + " " + v.id + "=" + v.value);
}

function get_element(id) {
  let did = prefix + id;
  let r = document.getElementById(did);
  return r;
}

function strip_prefix(id) {
  if (id.startsWith(prefix)) {
    return id.substr(prefix.length);
  }
  return id;
}