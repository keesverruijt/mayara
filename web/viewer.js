
const prefix = 'myrv_';

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
  myrv_radar = d[id];

  if (myrv_radar === undefined || myrv_radar.controls === undefined) {
    restart(id);
    return;
  }
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