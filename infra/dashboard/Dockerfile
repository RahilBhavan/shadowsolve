FROM nginxinc/nginx-unprivileged:1.27.1-alpine
COPY index.html styles.css browser-smoke.html /usr/share/nginx/html/
COPY src /usr/share/nginx/html/src
COPY fixtures /usr/share/nginx/html/fixtures
COPY infra/dashboard/nginx.conf /etc/nginx/conf.d/default.conf
