// Reports what the page receives through console.log, which m1-mixed prints with a
// "page console:" prefix, so input routing and metrics can be checked from its log.
const log = (message) => {
  console.log(message);
  document.getElementById("log").textContent = message;
};
const size = () => `dpr ${devicePixelRatio} viewport ${innerWidth}x${innerHeight}`;
addEventListener("mousedown", (e) => log(`mousedown client ${e.clientX},${e.clientY} button ${e.button}`));
addEventListener("keydown", (e) => log(`keydown key ${e.key} code ${e.code}`));
addEventListener("keyup", (e) => log(`keyup key ${e.key} code ${e.code}`));
addEventListener("focus", () => log("window focus"));
addEventListener("blur", () => log("window blur"));
addEventListener("resize", () => log(`resize: ${size()}`));
addEventListener("load", () => log(`load: ${size()}`));
