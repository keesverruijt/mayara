import van from "./van-1.5.2.debug.js";

const {a, li} = van.tags

const RadarEntry = (id, name) => li(a({href: "/client.html?id=" + id}, name))

function radarsLoaded(d) {
  let c = Object.keys(d).length;
  if (c > 0) {
    let r = document.getElementById("radars");
    r.innerHTML = "<div>" + c + " radars detected</div><ul></ul>";
    r = r.getElementsByTagName('ul')[0];
    Object.keys(d).sort().forEach(function(v, i) { van.add(r, RadarEntry(v, d[v].name)); });
  }

  setTimeout(loadRadars, 15000);
}

function loadRadars() {
  fetch('/v1/api/radars')
  .then(res => res.json())
  .then(out => radarsLoaded(out))
  .catch(err => setTimeout(loadRadars, 15000));
}

window.onload = function () {
  setTimeout(loadRadars, 1000);
}
