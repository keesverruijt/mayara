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
        { href: "/viewer.html?id=" + id + "&draw=alt" },
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

function interfacesLoaded(d) {
  let c = Object.keys(d.interfaces).length;
  if (c > 0) {
    let r = document.getElementById("interfaces");
    r.innerHTML = "<div>" + c + " interfaces detected</div><table></table>";
    r = r.getElementsByTagName("table")[0];

    let brands = ["Interface", ...d.brands];
    let hdr = van.add(r, tr());
    brands.forEach((v) => van.add(hdr, td({ class: "myr" }, v)));

    let interfaces = d.interfaces;
    if (interfaces) {
      console.log("interfaces", interfaces);
      Object.keys(interfaces).forEach(function (v, i) {
        let row = van.add(r, tr());

        van.add(row, td({ class: "myr" }, v));
        if (interfaces[v].status) {
          van.add(
            row,
            td(
              {
                class: "myr_error",
                colspan: d.brands.length,
              },
              interfaces[v].status
            )
          );
        } else {
          d.brands.forEach((b) => {
            let status = interfaces[v].listeners[b];
            let className =
              status == "Listening" || status == "Active" ? "myr" : "myr_error";
            van.add(row, td({ class: className }, status));
          });
        }
      });
    }
  }

  setTimeout(loadRadars, 15000);
}

function loadRadars() {
  fetch("/v1/api/radars")
    .then((res) => res.json())
    .then((out) => radarsLoaded(out))
    .catch((err) => setTimeout(loadRadars, 15000));
}

function loadInterfaces() {
  fetch("/v1/api/interfaces")
    .then((res) => res.json())
    .then((out) => interfacesLoaded(out))
    .catch((err) => setTimeout(loadInterfaces, 15000));
}

window.onload = function () {
  loadRadars();
  loadInterfaces();
};
