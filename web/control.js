export { loadRadar, registerRadarCallback };

import van from "./van-1.5.2.js";

const { div, label, input, button, select, option } = van.tags

const prefix = 'myr_';
const auto_postfix = '_auto';

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
     
const AutoButton = (id) =>
  div(
    input({ type: 'checkbox', class: 'myr_auto', id: prefix + id + auto_postfix, onchange: e => do_change_auto(e) }),
    label({ for: prefix + id + auto_postfix }, 'Auto'),
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
    this.element = get_element_by_server_id(id);
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
    this.element = get_element_by_server_id(id);
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
    let v = { id: '0', value: '0' };
    setControl(v);
    restart(id);
  }
  myr_webSocket.onmessage = (e) => {
    let v = JSON.parse(e.data);
    console.log("<- " + e.data);
    setControl(v);
    myr_no_response_timeout.setTimeout();
  }

  callbacks.forEach((value) => {
    let r = myr_radar;
    value(r);
  });
}

function setControl(v) {
  let i = get_element_by_server_id(v.id);
  let control = myr_controls[v.id];
  if (i && control) {
    i.value = v.value;
    console.log("<- " + control.name + " = " + v.value);
    let n = i.parentNode.querySelector('.myr_numeric');
    if (n) n.innerHTML = v.value;
    let d = i.parentNode.querySelector('.myr_description');
    if (d) {
      let description = (control.descriptions) ? control.descriptions[v.value] : undefined;
      if (!description && control.hasAutoAdjustable && v['auto']) {
        description = "A" + ((v.value > 0) ? "+" + v.value : "") + ((v.value < 0) ? v.value : "");
        if (n) {
          n.min = control.autoAdjustMinValue;
          n.max = control.autoAdjustMaxValue;
        }
      }
      if (!description) description = v.value;
      d.innerHTML = description;
    }
    if (control.hasAuto && 'auto' in v) {
      let checkbox = i.parentNode.querySelector('.myr_auto');
      if (checkbox) {
        checkbox.checked = v.auto;
      }
    }
    if (v.error) {
      myr_error_message.raise(v.error);
    }
  }
}

function buildControls() {
  let c = get_element_by_server_id('title');
  c.innerHTML = "";
  van.add(c, div(myr_radar.name + " Controls"));

  c = get_element_by_server_id('controls');
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
      get_element_by_server_id(k).setAttribute('readonly', 'true');
    } else if (v['isStringValue']) {
      van.add(get_element_by_server_id(k).parentNode, SetButton());
    }
    if (v['hasAuto']) {
      van.add(get_element_by_server_id(k).parentNode, AutoButton(k));
    }
  }
}

function do_change(e) {
  let v = e.target;
  let id = html_to_server_id(v.id);
  console.log("change " + e + " " + id + "=" + v.value);
  let message = { id: id, value: v.value };
  let checkbox = document.getElementById(v.id + auto_postfix);
  if (checkbox) {
    message.auto = checkbox.checked;
  }
  let cv = JSON.stringify(message);
  myr_webSocket.send(cv);
  console.log(myr_controls[id].name + "-> " + cv);
}

function do_change_auto(e) {
  let checkbox = e.target;
  let id = html_to_server_id(checkbox.id);
  let v = document.getElementById(auto_to_value_id(checkbox.id));
  console.log("change auto" + e + " " + id + "=" + v.value + " auto=" + checkbox.checked);
  let cv = JSON.stringify({ id: id, value: v.value, auto: checkbox.checked });
  myr_webSocket.send(cv);
  console.log(myr_controls[id].name + "-> " + cv);
}

function do_button(e) {
  let v = e.target.previousElementSibling;
  let id = html_to_server_id(v.id);
  console.log("set_button " + e + " " + id + "=" + v.value);
  let cv = JSON.stringify({ id: id, value: v.value });
  myr_webSocket.send(cv);
  console.log(myr_controls[id].name + "-> " + cv);
}

function do_input(e) {
  let v = e.target;
  console.log("input " + e + " " + v.id + "=" + v.value);
}

function get_element_by_server_id(id) {
  let did = prefix + id;
  let r = document.getElementById(did);
  return r;
}

function html_to_server_id(id) {
  let r = id;
  if (r.startsWith(prefix)) {
    r = r.substr(prefix.length);
  }
  if (r.endsWith(auto_postfix)) {
    r = r.substr(0, r.length - auto_postfix.length);
  }
  return r;
}

function auto_to_value_id(id) {
  let r = id;
  if (r.endsWith(auto_postfix)) {
    r = r.substr(0, r.length - auto_postfix.length);
  }
  return r;
}