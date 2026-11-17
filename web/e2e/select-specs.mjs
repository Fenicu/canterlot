// Выбор спек Playwright для быстрой проверки по карте областей web/src.
// Чистые функции здесь тестируются в tests/unit/select-specs.test.ts; этот же
// файл используется как CLI из scripts/check-web.sh.
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

// Под vitest import.meta.url — виртуальный URL, под CLI из check-web.sh — file:.
const moduleUrl = new URL('../', import.meta.url);
const webRoot = moduleUrl.protocol === 'file:' ? fileURLToPath(moduleUrl) : `${process.cwd()}/`;
const areasFile = moduleUrl.protocol === 'file:'
    ? fileURLToPath(new URL('./areas.json', import.meta.url))
    : `${process.cwd()}/e2e/areas.json`;
const SPEC = /(?:^|\/)e2e\/(?:ui|parity)\/.*\.spec\.ts$/;
const PROJECTS = ['phone', 'desktop'];
// Глобальный путь дёргает весь набор, но только на телефоне: desktop нужен только для точечных правок.
const GLOBAL_PROJECTS = ['phone'];
const AREA_PROJECTS = ['phone', 'desktop'];

const escape = value => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

export function globToRegExp(glob) {
    let pattern = '';
    for (let i = 0; i < glob.length; i++) {
        const char = glob[i];
        if (char === '*') {
            if (glob[i + 1] === '*') {
                i++;
                if (glob[i + 1] === '/') {
                    i++;
                    pattern += '(?:.*/)?';
                } else {
                    pattern += '.*';
                }
            } else {
                pattern += '[^/]*';
            }
        } else if (char === '?') {
            pattern += '[^/]';
        } else {
            pattern += escape(char);
        }
    }
    return new RegExp(`^${pattern}$`);
}

export function matchesGlob(glob, path) {
    return globToRegExp(glob).test(path);
}

export function findUncovered(paths, areas) {
    const globs = [...areas.global, ...areas.areas.flatMap(area => area.sources)];
    const regexps = globs.map(globToRegExp);
    return paths.filter(path => !regexps.some(regexp => regexp.test(path)));
}

export function selectSpecs(changed, areas, allSpecs) {
    const selected = new Map();
    const add = (spec, reason, projects) => {
        if (!selected.has(spec)) selected.set(spec, { reasons: [], projects: new Set() });
        const entry = selected.get(spec);
        if (!entry.reasons.includes(reason)) entry.reasons.push(reason);
        for (const project of projects) entry.projects.add(project);
    };
    const globalGlobs = areas.global.map(globToRegExp);
    const areaMatches = areas.areas.map(area => ({
        name: area.name,
        specs: area.specs,
        regexps: area.sources.map(globToRegExp)
    }));
    for (const path of changed) {
        if (SPEC.test(path)) {
            add(path, 'изменена спека', AREA_PROJECTS);
            continue;
        }
        if (globalGlobs.some(regexp => regexp.test(path))) {
            for (const spec of allSpecs) add(spec, 'глобальный путь', GLOBAL_PROJECTS);
            continue;
        }
        for (const area of areaMatches) {
            if (area.regexps.some(regexp => regexp.test(path))) {
                for (const spec of area.specs) add(spec, `область «${area.name}»`, AREA_PROJECTS);
            }
        }
    }
    return [...selected]
        .map(([path, entry]) => ({
            path,
            reasons: entry.reasons,
            projects: PROJECTS.filter(project => entry.projects.has(project))
        }))
        .sort((a, b) => a.path.localeCompare(b.path));
}

function listFiles(dir) {
    const files = [];
    const walk = current => {
        for (const name of readdirSync(current)) {
            const path = `${current}/${name}`;
            if (statSync(path).isDirectory()) walk(path);
            else files.push(path);
        }
    };
    walk(dir);
    return files;
}

const local = path => relative(webRoot, path).split(sep).join('/');

function allSpecs() {
    return [
        ...listFiles(`${webRoot}/e2e/ui`),
        ...listFiles(`${webRoot}/e2e/parity`)
    ].filter(path => path.endsWith('.spec.ts')).map(local);
}

const areas = () => JSON.parse(readFileSync(areasFile, 'utf8'));

const mode = process.argv[2];
if (process.argv[1]?.endsWith('e2e/select-specs.mjs')) {
if (mode === '--completeness') {
    const paths = [
        ...listFiles(`${webRoot}/src`),
        ...listFiles(`${webRoot}/static`)
    ].map(local);
    const uncovered = findUncovered(paths, areas());
    if (uncovered.length) {
        process.stderr.write(`areas.json: ${uncovered.length} файлов не покрыты областью или глобальным списком:\n`);
        for (const path of uncovered) process.stderr.write(`  ${path}\n`);
        process.exit(1);
    }
    process.stdout.write(`areas.json: покрыты все ${paths.length} файлов под src/ и static/\n`);
} else if (mode === '--select') {
    const changed = readFileSync(0, 'utf8').split('\n').map(path => path.trim()).filter(Boolean);
    const selected = selectSpecs(changed, areas(), allSpecs());
    for (const { path, reasons, projects } of selected) {
        process.stdout.write(`${path}\t${reasons.join('; ')}\t${projects.join(',')}\n`);
    }
} else {
    process.stderr.write('Использование: node e2e/select-specs.mjs --completeness | --select < changed-paths\n');
    process.exit(2);
}
}
