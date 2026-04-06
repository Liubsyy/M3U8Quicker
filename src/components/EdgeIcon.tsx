import Icon from "@ant-design/icons";
import type { CustomIconComponentProps } from "@ant-design/icons/lib/components/Icon";

const EdgeSvg = () => (
  <svg viewBox="0 0 48 48" width="1em" height="1em" fill="none">
    <path
      d="M41.6 31.3c0 7.7-6.3 13.9-14 13.9-8.4 0-15.1-6.9-15.1-15.3 0-8 6.6-14.6 14.6-14.6 6.1 0 11.6 3.8 13.8 9.4-1.6-1.6-4.1-3.2-8.1-3.2-6.8 0-11.9 5-11.9 10.5 0 5.2 4 8.9 8.4 8.9 4.1 0 7.5-2.2 8.6-5.8H25.8v-4.7h15.6c.1.4.2 1 .2.9Z"
      fill="currentColor"
    />
    <path
      d="M9.5 23.2c0-10.3 8.4-18.7 18.8-18.7 7.6 0 14.4 4.6 17.3 11.5-2.3-2.1-5.8-4.2-10.8-4.2-8.9 0-16.8 6.8-16.8 16.2 0 .8.1 1.7.3 2.5-5.1-.3-8.8-3-8.8-7.3Z"
      fill="currentColor"
      opacity="0.55"
    />
  </svg>
);

export function EdgeIcon(props: Partial<CustomIconComponentProps>) {
  return <Icon component={EdgeSvg} {...props} />;
}
