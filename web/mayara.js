import van from "./van-1.5.2.debug.js";

const { a, tr, td } = van.tags;

const RadarEntry = (id, name) =>
  tr(
    td(
      { class: "myr" },
      a({ href: "/control.html?id=" + id }, name + " controller")
    ),
    td(
      { class: "myr" },
      a({ href: "/viewer.html?id=" + id }, name + " PPI Viewer")
    ),
    td(
      { class: "myr" },
      a(
        { href: "/viewer.html?id=" + id + "&draw=gl" },
        name + " PPI (WebGL alt)"
      )
    ),
    td(
      { class: "myr" },
      a(
        { href: "/viewer.html?id=" + id + "&draw=2d" },
        name + " PPI (WebGL 2D)"
      )
    )
  );

function radarsLoaded(d) {
  let c = Object.keys(d).length;
  if (c > 0) {
    let r = document.getElementById("radars");
    r.innerHTML = "<div>" + c + " radars detected</div><table></table>";
    r = r.getElementsByTagName("table")[0];
    Object.keys(d)
      .sort()
      .forEach(function (v, i) {
        van.add(r, RadarEntry(v, d[v].name));
      });
  }

  setTimeout(loadRadars, 15000);
}

function loadRadars() {
  fetch("/v1/api/radars")
    .then((res) => res.json())
    .then((out) => radarsLoaded(out))
    .catch((err) => setTimeout(loadRadars, 15000));
}

window.onload = function () {
  loadRadars();
};
