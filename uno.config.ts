import { defineConfig, presetUno, presetIcons } from "unocss";

export default defineConfig({
  presets: [presetUno(), presetIcons()],
  theme: {
    colors: {
      surface: {
        DEFAULT: "#faf9f7",
        hover: "#f3f1ee",
      },
    },
  },
  shortcuts: {
    // Layout
    "app-shell": "h-screen flex flex-col select-none",
  },
});
