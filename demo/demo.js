(function () {
  'use strict';
  const places = [
    { id: 'century', name: '世纪公园', district: '浦东新区', category: 'park', mark: '♧', summary: '给周末留一段绿色。此处仅展示地点卡片与收藏交互。' },
    { id: 'gucun', name: '顾村公园', district: '宝山区', category: 'park', mark: '♧', summary: '把城市绿地放进待去清单，具体游玩信息需另行核实。' },
    { id: 'natural', name: '上海自然博物馆', district: '静安区', category: 'culture', mark: '⌂', summary: '留一个探索自然的灵感；票务、开放与预约信息未核验。' },
    { id: 'art', name: '中华艺术宫', district: '浦东新区', category: 'culture', mark: '⌂', summary: '把文化空间加入周末备选，不代表亲子体验推荐。' },
    { id: 'xuhui', name: '徐汇滨江', district: '徐汇区', category: 'waterfront', mark: '≈', summary: '沿江散步的地点示例；路线和现场条件请出发前确认。' },
    { id: 'yangpu', name: '杨浦滨江', district: '杨浦区', category: 'waterfront', mark: '≈', summary: '另一个滨水备选，未提供经核验的设施或适龄建议。' }
  ];
  const labels = { park: '公园绿地', culture: '文化探索', waterfront: '滨水散步' };
  const key = 'relay:p0:sample-favorites:v1';
  const validIds = new Set(places.map(place => place.id));
  let favorites = new Set();
  let category = 'all';
  let memoryOnly = false;
  function fallback() {
    memoryOnly = true;
    document.getElementById('storage-notice').textContent = '浏览器存储不可用，收藏已回退到内存；关闭或刷新页面后不保证保留。';
  }
  try {
    const raw = window.localStorage.getItem(key);
    if (raw !== null) {
      const parsed = JSON.parse(raw);
      if (!Array.isArray(parsed)) throw new Error('Invalid favorites');
      favorites = new Set(parsed.filter(id => validIds.has(id)));
    }
  } catch (_) { fallback(); }
  function save() {
    if (memoryOnly) return;
    try { window.localStorage.setItem(key, JSON.stringify([...favorites])); }
    catch (_) { fallback(); }
  }
  function node(tag, className, text) {
    const item = document.createElement(tag);
    if (className) item.className = className;
    if (text !== undefined) item.textContent = text;
    return item;
  }
  function render() {
    const query = document.getElementById('search').value.trim().toLowerCase();
    const favoritesOnly = document.getElementById('favorites-only').checked;
    const visible = places.filter(place => (!query || `${place.name} ${place.district} ${labels[place.category]}`.toLowerCase().includes(query)) &&
      (category === 'all' || place.category === category) && (!favoritesOnly || favorites.has(place.id)));
    const container = document.getElementById('places');
    container.replaceChildren();
    for (const place of visible) {
      const card = node('article', 'place-card');
      const art = node('div', 'place-art art-' + place.category);
      art.setAttribute('aria-hidden', 'true');
      art.append(node('span', '', place.mark));
      const text = node('div', 'place-text');
      const meta = node('div', 'place-meta');
      meta.append(node('span', '', place.district), node('span', 'place-category', labels[place.category]));
      const bottom = node('div', 'place-bottom');
      const button = node('button', 'favorite-button', favorites.has(place.id) ? '♥ 已收藏' : '♡ 收藏');
      button.type = 'button';
      button.setAttribute('aria-label', (favorites.has(place.id) ? '取消收藏 ' : '收藏 ') + place.name);
      button.setAttribute('aria-pressed', String(favorites.has(place.id)));
      button.addEventListener('click', () => {
        if (favorites.has(place.id)) favorites.delete(place.id);
        else favorites.add(place.id);
        save();
        render();
        const replacement = container.querySelector(`[data-place="${place.id}"] .favorite-button`);
        if (replacement) replacement.focus();
        else document.getElementById('favorites-only').focus();
      });
      bottom.append(node('span', 'pending', '待体验 · 信息待核实'), button);
      text.append(meta, node('h3', '', place.name), node('p', '', place.summary), bottom);
      card.dataset.place = place.id;
      card.append(art, text);
      container.append(card);
    }
    document.getElementById('result-count').textContent = visible.length + ' 个示例地点';
    document.getElementById('empty').hidden = visible.length !== 0;
    document.querySelectorAll('[data-category]').forEach(button => button.setAttribute('aria-pressed', String(button.dataset.category === category)));
  }
  document.getElementById('search').addEventListener('input', render);
  document.getElementById('favorites-only').addEventListener('change', render);
  document.querySelectorAll('[data-category]').forEach(button => button.addEventListener('click', () => { category = button.dataset.category; render(); }));
  render();
})();
