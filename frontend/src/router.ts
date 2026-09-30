import { createRouter, createWebHashHistory } from 'vue-router'
import type { RouteRecordRaw } from 'vue-router'
import TasksPage from '@/pages/TasksPage.vue'
import StatusPage from '@/pages/StatusPage.vue'
import SettingsPage from '@/pages/SettingsPage.vue'

export const routes: RouteRecordRaw[] = [
  { path: '/', redirect: '/tasks' },
  { path: '/tasks', component: TasksPage, meta: { title: 'Tasks' } },
  { path: '/status', component: StatusPage, meta: { title: 'Status' } },
  { path: '/settings', component: SettingsPage, meta: { title: 'Settings' } },
  { path: '/:pathMatch(.*)*', redirect: '/tasks' },
]

// Hash history: the Electron main process serves the bundle as static files
// over the app's own scheme, with no server-side rewrite, so the route must
// live in the URL fragment.
export const router = createRouter({
  history: createWebHashHistory(),
  routes,
})

export default router
