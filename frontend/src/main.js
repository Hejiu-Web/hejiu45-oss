import { createApp } from 'vue'
import App from './App.vue'
import router from './router.js'
import { locale, setLocale, t } from './i18n.js'
import './styles.css'

const app = createApp(App)

// `$t` in every template, and it re-renders when the locale changes because the
// message lookup reads the reactive `locale` ref.
app.config.globalProperties.$t = (key, params) => {
  // Touch the ref so Vue tracks it as a dependency of this render.
  void locale.value
  return t(key, params)
}

// Apply the detected/stored language to <html lang> on boot.
setLocale(locale.value)

app.use(router).mount('#app')
