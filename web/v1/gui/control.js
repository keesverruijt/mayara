export { loadRadar, registerRadarCallback, registerControlCallback };

import van from "/imports/van-1.5.2.min.js";
import { toUser } from "./units.js";

const { div, label, input, button, select, option } = van.tags;

const prefix = "myr_";
const auto_postfix = "_auto";
const enabled_postfix = "_enabled";

const RANGE_UNIT_SELECT_ID = 999;

var myr_radar;
var myr_controls;
var myr_range_control_id;
var myr_webSocket;
var myr_error_message;
var myr_no_response_timeout;
var myr_callbacks = Array();
var myr_control_callbacks = Array();
var myr_control_values = Array();

function registerRadarCallback(callback) {
  myr_callbacks.push(callback);
}

function registerControlCallback(callback) {
  myr_control_callbacks.push(callback);
}

const ReadOnlyValue = (id, name) =>
  div(
    { class: "myr_control myr_readonly" },
    div({ class: "myr_numeric", id: prefix + id }),
    div(name)
  );

const StringValue = (id, name) =>
  div(
    { class: "myr_control" },
    label({ for: prefix + id }, name),
    input({ type: "text", id: prefix + id, size: 20 })
  );

const NumericValue = (id, name) =>
  div(
    { class: "myr_control" },
    div({ class: "myr_numeric" }),
    label({ for: prefix + id }, name),
    input({
      type: "number",
      id: prefix + id,
      onchange: (e) => do_change(e.target),
      oninput: (e) => do_input(e),
    })
  );

// this is the HTML range control, not a radar range!
const RangeValue = (id, name, min, max, def, descriptions) =>
  div(
    { class: "myr_control" },
    div({ class: "myr_description" }),
    label({ for: prefix + id }, name),
    input({
      type: "range",
      id: prefix + id,
      min,
      max,
      value: def,
      onchange: (e) => do_change(e.target),
    })
  );

const ButtonValue = (id, name) =>
  div(
    { class: "myr_button" },
    button(
      { type: "button", id: prefix + id, onclick: (e) => do_change(e.target) },
      name
    )
  );

const AutoButton = (id) =>
  div(
    { class: "myr_button" },
    label({ for: prefix + id + auto_postfix, class: "myr_auto_label" }, "Auto"),
    input({
      type: "checkbox",
      class: "myr_auto",
      id: prefix + id + auto_postfix,
      onchange: (e) => do_change_auto(e.target),
    })
  );

const EnabledButton = (id) =>
  div(
    { class: "myr_button" },
    label(
      { for: prefix + id + enabled_postfix, class: "myr_enabled_label" },
      "Enabled"
    ),
    input({
      type: "checkbox",
      class: "myr_enabled",
      id: prefix + id + enabled_postfix,
      onchange: (e) => do_change_enabled(e.target),
    })
  );

const SelectValue = (id, name, validValues, descriptions) => {
  let r = div(
    { class: "myr_control" },
    label({ for: prefix + id }, name),
    div({ class: "myr_description" }),
    select(
      { id: prefix + id, onchange: (e) => do_change(e.target) },
      validValues.map((v) => option({ value: v }, descriptions[v]))
    )
  );
  return r;
};

const SetButton = () =>
  button({ type: "button", onclick: (e) => do_button(e) }, "Set");

class TemporaryMessage {
  timeoutId;
  element;

  constructor(id) {
    this.element = get_element_by_server_id(id);
  }

  raise(aMessage) {
    this.element.style.visibility = "visible";
    this.element.classList.remove("myr_vanish");
    this.element.innerHTML = aMessage;
    this.timeoutId = setTimeout(() => {
      this.cancel();
    }, 5000);
  }

  cancel() {
    if (typeof this.timeoutId === "number") {
      clearTimeout(this.timeoutId);
    }
    this.element.classList.add("myr_vanish");
  }
}

class Timeout {
  timeoutId;
  element;

  constructor(id) {
    this.element = get_element_by_server_id(id);
  }

  setTimeout() {
    this.cancel();
    this.timeoutId = setTimeout(() => {
      setControl({ id: "0", value: "0" });
    }, 15000);
  }

  cancel() {
    if (typeof this.timeoutId === "number") {
      clearTimeout(this.timeoutId);
      this.timeoutId = undefined;
    }
  }
}

//
// This is not called when used in a nested module.
//
window.onload = function () {
  const urlParams = new URLSearchParams(window.location.search);
  const id = urlParams.get("id");

  loadRadar(id);
};

function loadRadar(id) {
  fetch("/v1/api/radars")
    .then((res) => res.json())
    .then((out) => radarsLoaded(id, out))
    .catch((err) => restart(id));
}

function restart(id) {
  console.log("restart(" + id + ")");
  setTimeout(loadRadar, 15000, id);
}

function convertControlsToUserUnits(controls) {
  const result = {};

  Object.entries(controls).forEach(([id, ctrl]) => {
    // shallow clone – keep everything else intact
    let cloned = { ...ctrl };

    if (cloned.units) {
      let units = cloned.units;
      // Convert only numeric properties that exist
      ["minValue", "maxValue", "stepValue"].forEach((prop) => {
        if (prop in cloned) {
          [units, cloned[prop]] = toUser(cloned.units, cloned[prop]);
        }
      });
      cloned.user_units = units;
    }

    result[id] = cloned;
  });

  return result;
}

function radarsLoaded(id, d) {
  myr_radar = d[id];

  if (myr_radar === undefined || myr_radar.controls === undefined) {
    restart(id);
    return;
  }

  myr_controls = convertControlsToUserUnits(myr_radar.controls);
  myr_error_message = new TemporaryMessage("error");

  buildControls();
  myr_no_response_timeout = new Timeout("0");

  myr_webSocket = new WebSocket(myr_radar.controlUrl);

  myr_webSocket.onopen = (e) => {
    console.log("websocket open: " + JSON.stringify(e));
  };
  myr_webSocket.onclose = (e) => {
    console.log("websocket close: " + e);
    let v = { id: "0", value: "0" };
    setControl(v);
    restart(id);
  };
  myr_webSocket.onmessage = (e) => {
    let v = JSON.parse(e.data);
    console.log("<- " + e.data);
    setControl(v);
    myr_no_response_timeout.setTimeout();
  };

  myr_callbacks.forEach((cb) => {
    cb(myr_radar);
  });
}

/**
 * Rounds a number to a limited number of decimals, for user pleasure.
 *
 * @param {number} value      The value you want to round.
 * @param {number} stepValue  Step size – any positive float.
 * @returns {number}          Rounded result
 */
function roundToStep(value, stepValue) {
  value = Number(value);
  if (!Number.isFinite(value) || !Number.isFinite(stepValue)) return NaN;

  if (Math.abs(stepValue - 0.1) < Number.EPSILON) {
    return Number((value + stepValue / 2).toFixed(1));
  }
  if (stepValue < 0.02) {
    return Number((value + stepValue / 2).toFixed(2));
  }
  if (stepValue <= 1) {
    return Number((value + stepValue / 2).toFixed(1));
  }

  const scale = 1 / stepValue; // e.g. 100 for 0.01

  const scaledVal = Math.round(value * scale);
  const scaledStep = Math.round(stepValue * scale);

  const roundedInt = Math.round(scaledVal / scaledStep) * scaledStep;

  const rounded = roundedInt / scale;

  return rounded; // plain number otherwise
}

//
// When the websocket returns data from the server, it is assumed alwas to
// be a ControlValue. In V1 that is correct.
//

function setControl(cv) {
  myr_control_values[cv.id] = cv; // Save it for later when auto changes

  let i = get_element_by_server_id(cv.id);
  let control = myr_controls[cv.id];
  let units = undefined;
  var value;

  if (i && control) {
    var value;
    if (control.hasAutoAdjustable && cv.auto) {
      value = cv.autoValue;
      console.log("Using autoValue " + value);
    } else {
      value = cv.value;
    }

    if (cv.units && control.name != "Range") {
      [units, value] = toUser(cv.units, value);
      console.log(
        "<- " +
          control.name +
          " = si " +
          cv.value +
          control.units +
          " to user " +
          value +
          units
      );
      if (control.stepValue) {
        value = roundToStep(value, control.stepValue);

        console.log(
          "<- " +
            control.name +
            " = si " +
            cv.value +
            control.units +
            " to user " +
            value +
            units +
            " after step value " +
            control.stepValue
        );
      }
    } else {
      console.log("<- " + control.name + " = " + value);
    }

    let n = i.parentNode.querySelector(".myr_numeric");
    if (n) {
      if (units) {
        n.innerHTML = value + " " + units;
      } else {
        n.innerHTML = value;
      }
    }
    let d = i.parentNode.querySelector(".myr_description");
    if (d) {
      let description = control.descriptions
        ? control.descriptions[value]
        : undefined;
      if (!description && control.hasAutoAdjustable) {
        if (cv["auto"]) {
          description =
            "A" + (value > 0 ? "+" + value : "") + (value < 0 ? value : "");
          i.min = control.autoAdjustMinValue;
          i.max = control.autoAdjustMaxValue;
        } else {
          i.min = control.minValue;
          i.max = control.maxValue;
        }
      }
      if (!description) description = value;
      d.innerHTML = description;
    }
    // Set this after setting min, max
    i.value = value;
    console.log(
      "Control " + cv.id + " set to " + value + " min " + i.min,
      " max " + i.max
    );

    if (control.hasAuto && "auto" in cv) {
      let checkbox = i.parentNode.querySelector(".myr_auto");
      if (checkbox) {
        checkbox.checked = cv.auto;
      }
      let display = cv.auto && !control.hasAutoAdjustable ? "none" : "block";
      if (n) {
        n.style.display = display;
      }
      if (d) {
        d.style.display = display;
      }
      i.style.display = display;
    }

    if ("enabled" in cv) {
      let checkbox = i.parentNode.querySelector(".myr_enabled");
      if (checkbox) {
        checkbox.checked = cv.enabled;
      }
      let display = cv.enabled ? "block" : "none";
      if (n) {
        n.style.display = display;
      }
      if (d) {
        d.style.display = display;
      }
      i.style.display = display;
    }

    if (control.name == "Range") {
      myr_range_control_id = cv.id;

      let r = parseFloat(cv.value);
      if (control.descriptions && control.descriptions[r]) {
        let units = control.descriptions[r].split(/(\s+)/);
        // Filter either on 'nm' or 'm'
        if (units.length == 3) {
          let units_el = get_element_by_server_id(RANGE_UNIT_SELECT_ID);
          if (units_el) {
            let new_value = units[2] == "nm" ? 1 : 0;
            if (units_el.value != new_value) {
              // Only change if different
              units_el.value = new_value;
              handle_range_unit_change(new_value);
              i.value = cv.value;
            }
          }
        }
      }
    }

    if (cv.hasOwnProperty("allowed")) {
      let p = i.parentNode;
      if (!cv.allowed) {
        p.classList.add("myr_readonly");
        i.disabled = true;
      } else {
        p.classList.remove("myr_readonly");
        i.disabled = false;
      }
    }

    myr_control_callbacks.forEach((cb) => {
      cb(control, cv);
    });

    if (cv.error) {
      myr_error_message.raise(cv.error);
    }
  }
}

function buildControls() {
  let c = get_element_by_server_id("title");
  c.innerHTML = "";
  van.add(c, div(myr_radar.name + " Controls"));

  c = get_element_by_server_id("controls");
  c.innerHTML = "";
  for (const [k, v] of Object.entries(myr_controls)) {
    console.log("build control " + v);
    if (v["isReadOnly"]) {
      if (k == 0) {
        van.add(c, div({ class: "myr_control myr_error" }, "REPLAY MODE"));
      }
      van.add(c, ReadOnlyValue(k, v.name));
    } else if (v["dataType"] == "button") {
      van.add(c, ButtonValue(k, v.name));
    } else if (v["dataType"] == "string") {
      van.add(c, StringValue(k, v.name));
      van.add(get_element_by_server_id(k).parentNode, SetButton());
    } else if ("validValues" in v && "descriptions" in v) {
      if (v.name == "Range") {
        add_range_unit_select(c, v["descriptions"]);
      }
      van.add(c, SelectValue(k, v.name, v["validValues"], v["descriptions"]));
    } else if (
      "maxValue" in v &&
      v.maxValue <= 100 &&
      (!v.units || v.units !== "m/s") // "m/s" check makes DopplerSpeedThreshold show as normal number
    ) {
      van.add(
        c,
        RangeValue(k, v.name, v.minValue, v.maxValue, 0, "descriptions" in v)
      );
    } else {
      van.add(c, NumericValue(k, v.name));
    }
    if (v["hasAuto"]) {
      van.add(get_element_by_server_id(k).parentNode, AutoButton(k));
    }
    if (v["hasEnabled"] && !v["isReadOnly"]) {
      van.add(get_element_by_server_id(k).parentNode, EnabledButton(k));
    }
  }
}

function add_range_unit_select(c, descriptions) {
  let found_metric = false;
  let found_nautical = false;
  for (const [k, v] of Object.entries(descriptions)) {
    if (v.match(/ nm$/)) {
      found_nautical = true;
    } else {
      found_metric = true;
    }
  }
  if (found_metric && found_nautical) {
    van.add(
      c,
      SelectValue(RANGE_UNIT_SELECT_ID, "Range units", [0, 1], {
        0: "Metric",
        1: "Nautic",
      })
    );
  }
}

function do_change(v) {
  let id = html_to_server_id(v.id);
  if (id == RANGE_UNIT_SELECT_ID) {
    handle_range_unit_change(v.value);
    return;
  }
  let control = myr_controls[id];
  let update = myr_control_values[id];
  let message = { id: id };
  let value = v.value;
  if ("user_units" in control && control.name != "Range") {
    message.units = control.user_units;
    value = Number(value);
  }
  let checkbox = document.getElementById(v.id + auto_postfix);
  if (checkbox) {
    update.auto = checkbox.checked;
    message.auto = checkbox.checked;
  }
  if (checkbox && checkbox.checked && control.hasAutoAdjustable) {
    update.autoValue = value;
    message.autoValue = value;
  } else {
    update.value = value;
    message.value = value;
  }
  checkbox = document.getElementById(v.id + enabled_postfix);
  if (checkbox) {
    update.enabled = checkbox.checked;
    message.enabled = checkbox.checked;
  }
  setControl(update); // Update the GUI state so the proper Auto/NotAuto value is shown
  let cv = JSON.stringify(message);
  myr_webSocket.send(cv);
  console.log(myr_controls[id].name + "-> " + cv);
}

function do_change_auto(checkbox) {
  let id = html_to_server_id(checkbox.id);
  let v = document.getElementById(html_to_value_id(checkbox.id));

  let update = myr_control_values[id];
  update.auto = checkbox.checked;
  setControl(update); // Update the GUI state so the proper Auto/NotAuto value is shown

  let cv = JSON.stringify({ id: id, auto: checkbox.checked });
  myr_webSocket.send(cv);
  console.log(myr_controls[id].name + "-> " + cv);
}

function do_change_enabled(checkbox) {
  let id = html_to_server_id(checkbox.id);
  let v = document.getElementById(html_to_value_id(checkbox.id));
  do_change(v);
}

function do_button(e) {
  let v = e.target.previousElementSibling;
  let id = html_to_server_id(v.id);
  console.log("do_button " + e + " " + id);
  let cv = JSON.stringify({ id: id });
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
  return html_to_value_id(r);
}

function html_to_value_id(id) {
  let r = id;
  if (r.endsWith(auto_postfix)) {
    r = r.substr(0, r.length - auto_postfix.length);
  }
  if (r.endsWith(enabled_postfix)) {
    r = r.substr(0, r.length - enabled_postfix.length);
  }
  return r;
}

function handle_range_unit_change(value) {
  let units = value == 0 ? / (k?)m$/ : / nm$/;

  if (myr_range_control_id) {
    let e = get_element_by_server_id(myr_range_control_id);
    // Rebuild the select elements from scratch
    let c = myr_controls[myr_range_control_id];

    let validValues = Array();
    let descriptions = {};

    for (const r of c.validValues) {
      if (c.descriptions[r].match(units)) {
        validValues.push(r);
        descriptions[r] = c.descriptions[r];
      }
    }

    e.innerHTML = "";
    van.add(
      e,
      validValues.map((v) => option({ value: v }, descriptions[v]))
    );
  }
}
