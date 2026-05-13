(() => {
  // Maps a hostname pattern to a script that should be injected into the page (MAIN world).
  // Add a new site by appending an entry here and dropping its script next to this file.
  globalThis.__m3u8quickerInjects = [
    {
      id: "bilibili",
      hostPattern: /\.bilibili\.com$/i,
      script: "injects/bilibili.js",
      flag: "m3u8quickerBilibiliInjected",
    },
    {
      id: "douyin",
      hostPattern: /(^|\.)douyin\.com$/i,
      script: "injects/douyin.js",
      flag: "m3u8quickerDouyinInjected",
    },
    {
      id: "cctv",
      hostPattern: /(^|\.)((cntv|cctv)\.(com|cn)|ncpa-classic\.com)$/i,
      script: "injects/cctv.js",
      flag: "m3u8quickerCctvInjected",
    },
  ];
})();
