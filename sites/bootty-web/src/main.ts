import { createRustSiteBackend, mountCanvasTerminal, rustSiteNavigation, type RustSiteNavigationItem } from "bootty.js/browser";
import "./style.css";

let routes = new Map<string, string>();

const canvasElement = document.querySelector<HTMLCanvasElement>("#terminal");
if (!canvasElement) {
  throw new Error("Bootty site canvas is missing");
}
const canvas = canvasElement;
const fpsCounter = document.querySelector<HTMLElement>("#fps-counter");
let frames = 0;
let fpsStartedAt = performance.now();

await Promise.all([document.fonts.load('18px "Fira Code"'), document.fonts.load('18px "Maple Mono NF"')]);
await document.fonts.ready;

const siteNavigation = await rustSiteNavigation();
renderSiteNavigation(siteNavigation);
routes = routeMap(siteNavigation);

let mounted = await mountPage(routeFromLocation());

document.addEventListener("click", (event) => {
  const link = (event.target as Element | null)?.closest<HTMLAnchorElement>("a[data-route]");
  if (!link) {
    return;
  }
  const url = new URL(link.href);
  if (url.origin !== location.origin) {
    return;
  }
  event.preventDefault();
  void navigate(url.pathname);
});

window.addEventListener("popstate", () => {
  void showPage(routeFromLocation());
});

async function navigate(pathname: string): Promise<void> {
  if (pathname !== location.pathname) {
    history.pushState(null, "", pathname);
  }
  await showPage(routeFromLocation());
}

async function showPage(page: string): Promise<void> {
  setActiveNav(page);
  mounted.dispose();
  mounted = await mountPage(page);
}

async function mountPage(page: string) {
  setActiveNav(page);
  return mountCanvasTerminal({
    canvas,
    backend: () => createRustSiteBackend({ page }),
    autoFocus: false,
    onError(error) {
      console.error(error);
    },
    onFrame() {
      publishFps();
    },
  });
}

function publishFps(): void {
  frames += 1;
  const now = performance.now();
  const elapsed = now - fpsStartedAt;
  if (elapsed < 1000) {
    return;
  }
  if (fpsCounter) {
    fpsCounter.textContent = `${((frames * 1000) / elapsed).toFixed(1).padStart(5, "0")} fps`;
  }
  frames = 0;
  fpsStartedAt = now;
}

function routeFromLocation(): string {
  return routes.get(location.pathname.replace(/\/$/, "") || "/") ?? "overview";
}

function routeMap(items: RustSiteNavigationItem[]): Map<string, string> {
  const map = new Map(items.flatMap((item) => [[item.path, item.slug], [item.slug === "overview" ? "/overview" : `/${item.slug}`, item.slug]]));
  map.set("/javascript", "docs");
  map.set("/rust", "docs");
  return map;
}

function renderSiteNavigation(items: RustSiteNavigationItem[]): void {
  const sidebar = document.querySelector<HTMLElement>(".site-sidebar");
  if (sidebar) {
    sidebar.replaceChildren(...items.map((item) => navLink(item)));
  }
  const footer = document.querySelector<HTMLElement>(".site-footer nav");
  if (footer) {
    footer.replaceChildren(
      footerLink("npm", "https://www.npmjs.com/package/bootty.js", npmIcon()),
      footerLink("GitHub", "https://github.com/majinboos/bootty", githubIcon()),
    );
  }
}

function navLink(item: RustSiteNavigationItem): HTMLAnchorElement {
  const link = document.createElement("a");
  link.href = item.path;
  link.dataset.route = item.slug;
  link.append(navIcon(item.slug));
  const label = document.createElement("span");
  label.textContent = item.label;
  link.append(label);
  return link;
}

function navIcon(slug: string): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "nav-icon");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.innerHTML =
    {
      overview: '<path d="M4 6.5A2.5 2.5 0 0 1 6.5 4h11A2.5 2.5 0 0 1 20 6.5v11a2.5 2.5 0 0 1-2.5 2.5h-11A2.5 2.5 0 0 1 4 17.5z"/><path d="m8 9 3 3-3 3"/><path d="M13 15h3.5"/>',
      quickstart: '<path d="M13 2 4 14h7l-1 8 9-12h-7z"/>',
      docs: '<path d="M6 4h9l3 3v13H6z"/><path d="M14 4v4h4"/><path d="M9 12h6"/><path d="M9 16h6"/>',
      renderer: '<rect x="4" y="5" width="16" height="12" rx="2"/><path d="m8 9 2.5 2.5L8 14"/><path d="M13 14h3"/><path d="M9 20h6"/>',
      config: '<circle cx="12" cy="12" r="3"/><path d="M12 2v3"/><path d="M12 19v3"/><path d="m4.9 4.9 2.1 2.1"/><path d="m17 17 2.1 2.1"/><path d="M2 12h3"/><path d="M19 12h3"/><path d="m4.9 19.1 2.1-2.1"/><path d="m17 7 2.1-2.1"/>',
    }[slug] ?? '<circle cx="12" cy="12" r="8"/>';
  return svg;
}

function footerLink(label: string, href: string, icon: SVGSVGElement): HTMLAnchorElement {
  const link = document.createElement("a");
  link.href = href;
  link.rel = "noreferrer";
  link.append(icon, document.createTextNode(label));
  return link;
}

function footerIcon(viewBox: string, path: string): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("viewBox", viewBox);
  svg.setAttribute("class", "footer-icon");
  svg.innerHTML = path;
  return svg;
}

function npmIcon(): SVGSVGElement {
  return footerIcon("0 0 24 24", '<path d="M3 7h18v10H3z"/><path d="M7 15V9h3v6"/><path d="M14 15V9h3v6"/>');
}

function githubIcon(): SVGSVGElement {
  return footerIcon(
    "0 0 24 24",
    '<path d="M12 2a10 10 0 0 0-3.2 19.5c.5.1.7-.2.7-.5v-1.8c-2.8.6-3.4-1.2-3.4-1.2-.5-1.1-1.1-1.4-1.1-1.4-.9-.6.1-.6.1-.6 1 .1 1.6 1.1 1.6 1.1.9 1.6 2.5 1.1 3.1.8.1-.7.4-1.1.7-1.4-2.2-.3-4.6-1.1-4.6-4.9 0-1.1.4-2 1.1-2.7-.1-.3-.5-1.3.1-2.7 0 0 .9-.3 2.8 1a9.8 9.8 0 0 1 5.2 0c1.9-1.3 2.8-1 2.8-1 .6 1.4.2 2.4.1 2.7.7.7 1.1 1.6 1.1 2.7 0 3.8-2.4 4.6-4.6 4.9.4.3.8 1 .8 2.1V21c0 .3.2.6.8.5A10 10 0 0 0 12 2z"/>',
  );
}

function setActiveNav(page: string): void {
  document.querySelectorAll<HTMLAnchorElement>("a[data-route]").forEach((link) => {
    const active = link.dataset.route === page;
    link.toggleAttribute("aria-current", active);
    link.classList.toggle("is-active", active);
  });
}
